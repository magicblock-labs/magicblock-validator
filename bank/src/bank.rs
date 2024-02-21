// FIXME: once we worked this out
#![allow(dead_code)]
#![allow(unused_variables)]

use std::{
    borrow::Cow,
    collections::HashSet,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, RwLock,
    },
};

use crate::{bank_rc::BankRc, builtins::BuiltinPrototype, transaction_batch::TransactionBatch};
use crate::{
    transaction_logs::{
        TransactionLogCollector, TransactionLogCollectorConfig, TransactionLogCollectorFilter,
        TransactionLogInfo,
    },
    transaction_results::LoadAndExecuteTransactionsOutput,
};
use log::{debug, info};
use solana_accounts_db::{
    accounts::Accounts,
    accounts_db::{AccountShrinkThreshold, AccountsDb, AccountsDbConfig},
    accounts_index::{AccountSecondaryIndexes, ZeroLamport},
    accounts_update_notifier_interface::AccountsUpdateNotifier,
    ancestors::Ancestors,
    blockhash_queue::BlockhashQueue,
    storable_accounts::StorableAccounts,
    transaction_results::TransactionExecutionDetails,
};
use solana_measure::measure::Measure;
use solana_program_runtime::{
    loaded_programs::{BlockRelation, ForkGraph, LoadedPrograms},
    timings::{ExecuteTimingType, ExecuteTimings},
};
use solana_sdk::{
    account::{AccountSharedData, ReadableAccount},
    clock::{Epoch, Slot, UnixTimestamp},
    epoch_schedule::EpochSchedule,
    feature_set::FeatureSet,
    fee::FeeStructure,
    fee_calculator::FeeRateGovernor,
    genesis_config::GenesisConfig,
    hash::Hash,
    nonce_info::NoncePartial,
    pubkey::Pubkey,
    rent_collector::RentCollector,
    transaction::{Result, SanitizedTransaction, TransactionError, MAX_TX_ACCOUNT_LOCKS},
};
use solana_svm::{account_loader::TransactionCheckResult, account_overrides::AccountOverrides};
use solana_svm::{runtime_config::RuntimeConfig, transaction_processor::TransactionBatchProcessor};
use solana_svm::{
    transaction_error_metrics::TransactionErrorMetrics,
    transaction_processor::TransactionProcessingCallback,
};

// -----------------
// ForkGraph
// -----------------
#[derive(Default)]
pub struct SimpleForkGraph;

impl ForkGraph for SimpleForkGraph {
    /// Returns the BlockRelation of A to B
    fn relationship(&self, a: Slot, b: Slot) -> BlockRelation {
        BlockRelation::Unrelated
    }

    /// Returns the epoch of the given slot
    fn slot_epoch(&self, _slot: Slot) -> Option<Epoch> {
        Some(0)
    }
}

// -----------------
// Bank
// -----------------
#[derive(Debug)]
pub struct Bank {
    /// References to accounts, parent and signature status
    pub rc: BankRc,

    /// Bank slot (i.e. block)
    slot: Slot,

    /// Bank epoch
    epoch: Epoch,

    /// initialized from genesis
    pub(crate) epoch_schedule: EpochSchedule,

    /// Transaction fee structure
    pub fee_structure: FeeStructure,

    /// Optional config parameters that can override runtime behavior
    pub(crate) runtime_config: Arc<RuntimeConfig>,

    builtin_programs: HashSet<Pubkey>,

    pub loaded_programs_cache: Arc<RwLock<LoadedPrograms<SimpleForkGraph>>>,

    transaction_processor: TransactionBatchProcessor<SimpleForkGraph>,

    // Global configuration for how transaction logs should be collected across all banks
    pub transaction_log_collector_config: Arc<RwLock<TransactionLogCollectorConfig>>,

    // Logs from transactions that this Bank executed collected according to the criteria in
    // `transaction_log_collector_config`
    pub transaction_log_collector: Arc<RwLock<TransactionLogCollector>>,

    transaction_debug_keys: Option<Arc<HashSet<Pubkey>>>,

    // -----------------
    // Genesis related
    // -----------------
    /// Total capitalization, used to calculate inflation
    capitalization: AtomicU64,

    /// The initial accounts data size at the start of this Bank, before processing any transactions/etc
    pub(super) accounts_data_size_initial: u64,

    /// Track cluster signature throughput and adjust fee rate
    pub(crate) fee_rate_governor: FeeRateGovernor,
    //
    // Bank max_tick_height
    max_tick_height: u64,

    /// The number of hashes in each tick. None value means hashing is disabled.
    hashes_per_tick: Option<u64>,

    /// The number of ticks in each slot.
    ticks_per_slot: u64,

    /// length of a slot in ns
    pub ns_per_slot: u128,

    /// genesis time, used for computed clock
    genesis_creation_time: UnixTimestamp,

    /// The number of slots per year, used for inflation
    slots_per_year: f64,

    // -----------------
    // For TransactionProcessingCallback
    // -----------------
    pub feature_set: Arc<FeatureSet>,

    /// latest rent collector, knows the epoch
    rent_collector: RentCollector,

    /// FIFO queue of `recent_blockhash` items
    blockhash_queue: RwLock<BlockhashQueue>,

    /// The set of parents including this bank
    /// NOTE: we're planning to only have this bank
    pub ancestors: Ancestors,
}

// -----------------
// TransactionProcessingCallback
// -----------------
impl TransactionProcessingCallback for Bank {
    fn account_matches_owners(&self, account: &Pubkey, owners: &[Pubkey]) -> Option<usize> {
        self.rc
            .accounts
            .accounts_db
            .account_matches_owners(&self.ancestors, account, owners)
            .ok()
    }

    fn get_account_shared_data(&self, pubkey: &Pubkey) -> Option<AccountSharedData> {
        self.rc
            .accounts
            .accounts_db
            .load_with_fixed_root(&self.ancestors, pubkey)
            .map(|(acc, _)| acc)
    }

    fn get_last_blockhash_and_lamports_per_signature(&self) -> (Hash, u64) {
        self.last_blockhash_and_lamports_per_signature()
    }

    fn get_rent_collector(&self) -> &RentCollector {
        &self.rent_collector
    }

    fn get_feature_set(&self) -> Arc<FeatureSet> {
        self.feature_set.clone()
    }

    fn check_account_access(
        &self,
        _tx: &SanitizedTransaction,
        _account_index: usize,
        _account: &AccountSharedData,
        _error_counters: &mut TransactionErrorMetrics,
    ) -> Result<()> {
        // NOTE: removed check for reward status/solana_stake_program::check_id(account.owner())
        // See: `bank.rs: 7519`
        Ok(())
    }
}

#[derive(Default)]
pub struct TransactionExecutionRecordingOpts {
    enable_cpi_recording: bool,
    enable_log_recording: bool,
    enable_return_data_recording: bool,
}

impl Bank {
    #[allow(clippy::too_many_arguments)]
    pub fn new_with_paths(
        genesis_config: &GenesisConfig,
        runtime_config: Arc<RuntimeConfig>,
        paths: Vec<PathBuf>,
        debug_keys: Option<Arc<HashSet<Pubkey>>>,
        additional_builtins: Option<&[BuiltinPrototype]>,
        account_indexes: AccountSecondaryIndexes,
        shrink_ratio: AccountShrinkThreshold,
        debug_do_not_add_builtins: bool,
        accounts_db_config: Option<AccountsDbConfig>,
        accounts_update_notifier: Option<AccountsUpdateNotifier>,
        #[allow(unused)] collector_id_for_tests: Option<Pubkey>,
        exit: Arc<AtomicBool>,
    ) -> Self {
        let accounts_db = AccountsDb::new_with_config(
            paths,
            &genesis_config.cluster_type,
            account_indexes,
            shrink_ratio,
            accounts_db_config,
            accounts_update_notifier,
            exit,
        );

        let accounts = Accounts::new(Arc::new(accounts_db));
        let mut bank = Self::default_with_accounts(accounts);
        bank.ancestors = Ancestors::from(vec![bank.slot()]);
        bank.transaction_debug_keys = debug_keys;
        bank.runtime_config = runtime_config;

        #[cfg(not(feature = "dev-context-only-utils"))]
        bank.process_genesis_config(genesis_config);
        #[cfg(feature = "dev-context-only-utils")]
        bank.process_genesis_config(genesis_config, collector_id_for_tests);

        // NOTE: leaving out finish_init which we need (at least the adding of builtins)

        // NOTE: leaving out stakes related setup

        // NOTE: leaving out clock, epoch_schedule, rent, slot, blockhash, sysvar setup
        bank
    }

    pub(super) fn default_with_accounts(accounts: Accounts) -> Self {
        // NOTE: this was not part of the original implementation
        let simple_fork_graph = Arc::<RwLock<SimpleForkGraph>>::default();
        let loaded_programs_cache = {
            // TODO: not sure how this is setup more proper in the original implementation
            // since there we don't call `set_fork_graph` directly from the bank
            // Also we don't need bank forks in our initial implementation
            let mut loaded_programs = LoadedPrograms::new(Slot::default(), Epoch::default());
            let simple_fork_graph = Arc::<RwLock<SimpleForkGraph>>::default();
            loaded_programs.set_fork_graph(simple_fork_graph);

            Arc::new(RwLock::new(loaded_programs))
        };

        let mut bank = Self {
            rc: BankRc::new(accounts, Slot::default()),
            slot: Slot::default(),
            epoch: Epoch::default(),
            epoch_schedule: EpochSchedule::default(),
            builtin_programs: HashSet::<Pubkey>::default(),
            runtime_config: Arc::<RuntimeConfig>::default(),
            transaction_debug_keys: Option::<Arc<HashSet<Pubkey>>>::default(),
            transaction_log_collector_config: Arc::<RwLock<TransactionLogCollectorConfig>>::default(
            ),
            transaction_log_collector: Arc::<RwLock<TransactionLogCollector>>::default(),
            fee_structure: FeeStructure::default(),
            loaded_programs_cache,
            transaction_processor: TransactionBatchProcessor::default(),

            // Genesis related
            accounts_data_size_initial: 0,
            capitalization: AtomicU64::default(),
            fee_rate_governor: FeeRateGovernor::default(),
            max_tick_height: u64::default(),
            hashes_per_tick: Option::<u64>::default(),
            ticks_per_slot: u64::default(),
            ns_per_slot: u128::default(),
            genesis_creation_time: UnixTimestamp::default(),
            slots_per_year: f64::default(),

            // For TransactionProcessingCallback
            ancestors: Ancestors::default(),
            blockhash_queue: RwLock::<BlockhashQueue>::default(),
            feature_set: Arc::<FeatureSet>::default(),
            rent_collector: RentCollector::default(),
        };

        bank.transaction_processor = TransactionBatchProcessor::new(
            bank.slot,
            bank.epoch,
            bank.epoch_schedule.clone(),
            bank.fee_structure.clone(),
            bank.runtime_config.clone(),
            bank.loaded_programs_cache.clone(),
        );

        bank
    }

    // -----------------
    // Genesis
    // -----------------
    fn process_genesis_config(
        &mut self,
        genesis_config: &GenesisConfig,
        #[cfg(feature = "dev-context-only-utils")] collector_id_for_tests: Option<Pubkey>,
    ) {
        // Bootstrap validator collects fees until `new_from_parent` is called.
        self.fee_rate_governor = genesis_config.fee_rate_governor.clone();

        for (pubkey, account) in genesis_config.accounts.iter() {
            // TODO: get_account
            // assert!(
            //     self.get_account(pubkey).is_none(),
            //     "{pubkey} repeated in genesis config"
            // );
            self.store_account(pubkey, account);
            self.capitalization
                .fetch_add(account.lamports(), Ordering::Relaxed);
            self.accounts_data_size_initial += account.data().len() as u64;
        }

        self.blockhash_queue.write().unwrap().genesis_hash(
            &genesis_config.hash(),
            self.fee_rate_governor.lamports_per_signature,
        );

        self.hashes_per_tick = genesis_config.hashes_per_tick();
        self.ticks_per_slot = genesis_config.ticks_per_slot();
        self.ns_per_slot = genesis_config.ns_per_slot();
        self.genesis_creation_time = genesis_config.creation_time;
        self.max_tick_height = (self.slot + 1) * self.ticks_per_slot;
        self.slots_per_year = genesis_config.slots_per_year();

        self.epoch_schedule = genesis_config.epoch_schedule.clone();

        // Add additional builtin programs specified in the genesis config
        // TODO: add_builtin_account
        // for (name, program_id) in &genesis_config.native_instruction_processors {
        //     self.add_builtin_account(name, program_id, false);
        // }
    }

    // -----------------
    // Slot and Epoch
    // -----------------
    pub fn slot(&self) -> Slot {
        self.slot
    }

    // -----------------
    // Blockhash and Lamports
    // -----------------
    pub fn last_blockhash_and_lamports_per_signature(&self) -> (Hash, u64) {
        let blockhash_queue = self.blockhash_queue.read().unwrap();
        let last_hash = blockhash_queue.last_hash();
        let last_lamports_per_signature = blockhash_queue
            .get_lamports_per_signature(&last_hash)
            .unwrap(); // safe so long as the BlockhashQueue is consistent
        (last_hash, last_lamports_per_signature)
    }

    /// Return the last block hash registered.
    pub fn last_blockhash(&self) -> Hash {
        self.blockhash_queue.read().unwrap().last_hash()
    }

    // -----------------
    // Accounts
    // -----------------
    /// fn store the single `account` with `pubkey`.
    /// Uses `store_accounts`, which works on a vector of accounts.
    pub fn store_account<T: ReadableAccount + Sync + ZeroLamport>(
        &self,
        pubkey: &Pubkey,
        account: &T,
    ) {
        self.store_accounts((self.slot(), &[(pubkey, account)][..]))
    }

    pub fn store_accounts<'a, T: ReadableAccount + Sync + ZeroLamport + 'a>(
        &self,
        accounts: impl StorableAccounts<'a, T>,
    ) {
        // NOTE: ideally we only have one bank and never freeze it
        // assert!(!self.freeze_started());
        //
        let mut m = Measure::start("stakes_cache.check_and_store");

        /* NOTE: for now disabled this part since we don't support staking
        let new_warmup_cooldown_rate_epoch = self.new_warmup_cooldown_rate_epoch();
        (0..accounts.len()).for_each(|i| {
            self.stakes_cache.check_and_store(
                accounts.pubkey(i),
                accounts.account(i),
                new_warmup_cooldown_rate_epoch,
            )
        });
        */
        self.rc.accounts.store_accounts_cached(accounts);
        m.stop();
        self.rc
            .accounts
            .accounts_db
            .stats
            .stakes_cache_check_and_store_us
            .fetch_add(m.as_us(), Ordering::Relaxed);
    }

    // -----------------
    // Transaction Accounts
    // -----------------
    pub fn unlock_accounts(&self, batch: &mut TransactionBatch) {
        if batch.needs_unlock() {
            batch.set_needs_unlock(false);
            self.rc
                .accounts
                .unlock_accounts(batch.sanitized_transactions().iter(), batch.lock_results())
        }
    }
    /// Get the max number of accounts that a transaction may lock in this block
    pub fn get_transaction_account_lock_limit(&self) -> usize {
        if let Some(transaction_account_lock_limit) =
            self.runtime_config.transaction_account_lock_limit
        {
            transaction_account_lock_limit
        } else {
            MAX_TX_ACCOUNT_LOCKS
        }
    }

    // -----------------
    // Transaction Preparation
    // -----------------
    /// Prepare a locked transaction batch from a list of sanitized transactions.
    pub fn prepare_sanitized_batch<'a, 'b>(
        &'a self,
        txs: &'b [SanitizedTransaction],
    ) -> TransactionBatch<'a, 'b> {
        let tx_account_lock_limit = self.get_transaction_account_lock_limit();
        let lock_results = self
            .rc
            .accounts
            .lock_accounts(txs.iter(), tx_account_lock_limit);
        TransactionBatch::new(lock_results, self, Cow::Borrowed(txs))
    }

    // -----------------
    // Transaction Checking
    // -----------------
    pub fn check_transactions(
        &self,
        sanitized_txs: &[impl core::borrow::Borrow<SanitizedTransaction>],
        lock_results: &[Result<()>],
        max_age: usize,
        error_counters: &mut TransactionErrorMetrics,
    ) -> Vec<TransactionCheckResult> {
        // Show that the check_timings measure works
        std::thread::sleep(std::time::Duration::from_millis(1));

        // FIXME: skipping check_transactions runtime/src/bank.rs: 4505 (which is mostly tx age related)
        sanitized_txs
            .iter()
            .map(|_| {
                let res: TransactionCheckResult = (Ok(()), None::<NoncePartial>, None);
                res
            })
            .collect::<Vec<_>>()
    }

    // -----------------
    // Transaction Execution
    // -----------------
    pub fn load_and_execute_transactions(
        &self,
        batch: &TransactionBatch,
        max_age: usize,
        recording_opts: TransactionExecutionRecordingOpts,
        timings: &mut ExecuteTimings,
        account_overrides: Option<&AccountOverrides>,
        log_messages_bytes_limit: Option<usize>,
    ) -> LoadAndExecuteTransactionsOutput {
        // 1. Extract and check sanitized transactions
        let sanitized_txs = batch.sanitized_transactions();
        debug!("processing transactions: {}", sanitized_txs.len());

        let mut error_counters = TransactionErrorMetrics::default();

        let retryable_transaction_indexes: Vec<_> = batch
            .lock_results()
            .iter()
            .enumerate()
            .filter_map(|(index, res)| match res {
                // following are retryable errors
                Err(TransactionError::AccountInUse) => {
                    error_counters.account_in_use += 1;
                    Some(index)
                }
                Err(TransactionError::WouldExceedMaxBlockCostLimit) => {
                    error_counters.would_exceed_max_block_cost_limit += 1;
                    Some(index)
                }
                Err(TransactionError::WouldExceedMaxVoteCostLimit) => {
                    error_counters.would_exceed_max_vote_cost_limit += 1;
                    Some(index)
                }
                Err(TransactionError::WouldExceedMaxAccountCostLimit) => {
                    error_counters.would_exceed_max_account_cost_limit += 1;
                    Some(index)
                }
                Err(TransactionError::WouldExceedAccountDataBlockLimit) => {
                    error_counters.would_exceed_account_data_block_limit += 1;
                    Some(index)
                }
                // following are non-retryable errors
                Err(TransactionError::TooManyAccountLocks) => {
                    error_counters.too_many_account_locks += 1;
                    None
                }
                Err(_) => None,
                Ok(_) => None,
            })
            .collect();

        let mut check_time = Measure::start("check_transactions");
        let mut check_results = self.check_transactions(
            sanitized_txs,
            batch.lock_results(),
            max_age,
            &mut error_counters,
        );
        check_time.stop();
        debug!("check: {}us", check_time.as_us());
        timings.saturating_add_in_place(ExecuteTimingType::CheckUs, check_time.as_us());

        // 2. Load and execute sanitized transactions
        let sanitized_output = self
            .transaction_processor
            .load_and_execute_sanitized_transactions(
                self,
                sanitized_txs,
                &mut check_results,
                &mut error_counters,
                recording_opts.enable_cpi_recording,
                recording_opts.enable_log_recording,
                recording_opts.enable_return_data_recording,
                timings,
                account_overrides,
                self.builtin_programs.iter(),
                log_messages_bytes_limit,
            );

        // 3. Record transaction execution stats
        let mut signature_count = 0;

        let mut executed_transactions_count: usize = 0;
        let mut executed_non_vote_transactions_count: usize = 0;
        let mut executed_with_successful_result_count: usize = 0;
        let err_count = &mut error_counters.total;
        let transaction_log_collector_config =
            self.transaction_log_collector_config.read().unwrap();

        let mut collect_logs_time = Measure::start("collect_logs_time");
        for (execution_result, tx) in sanitized_output.execution_results.iter().zip(sanitized_txs) {
            if let Some(debug_keys) = &self.transaction_debug_keys {
                for key in tx.message().account_keys().iter() {
                    if debug_keys.contains(key) {
                        let result = execution_result.flattened_result();
                        info!("slot: {} result: {:?} tx: {:?}", self.slot, result, tx);
                        break;
                    }
                }
            }

            let is_vote = tx.is_simple_vote_transaction();

            if execution_result.was_executed() // Skip log collection for unprocessed transactions
                && transaction_log_collector_config.filter != TransactionLogCollectorFilter::None
            {
                let mut filtered_mentioned_addresses = Vec::new();
                if !transaction_log_collector_config
                    .mentioned_addresses
                    .is_empty()
                {
                    for key in tx.message().account_keys().iter() {
                        if transaction_log_collector_config
                            .mentioned_addresses
                            .contains(key)
                        {
                            filtered_mentioned_addresses.push(*key);
                        }
                    }
                }

                let store = match transaction_log_collector_config.filter {
                    TransactionLogCollectorFilter::All => {
                        !is_vote || !filtered_mentioned_addresses.is_empty()
                    }
                    TransactionLogCollectorFilter::AllWithVotes => true,
                    TransactionLogCollectorFilter::None => false,
                    TransactionLogCollectorFilter::OnlyMentionedAddresses => {
                        !filtered_mentioned_addresses.is_empty()
                    }
                };

                if store {
                    if let Some(TransactionExecutionDetails {
                        status,
                        log_messages: Some(log_messages),
                        ..
                    }) = execution_result.details()
                    {
                        let mut transaction_log_collector =
                            self.transaction_log_collector.write().unwrap();
                        let transaction_log_index = transaction_log_collector.logs.len();

                        transaction_log_collector.logs.push(TransactionLogInfo {
                            signature: *tx.signature(),
                            result: status.clone(),
                            is_vote,
                            log_messages: log_messages.clone(),
                        });
                        for key in filtered_mentioned_addresses.into_iter() {
                            transaction_log_collector
                                .mentioned_address_map
                                .entry(key)
                                .or_default()
                                .push(transaction_log_index);
                        }
                    }
                }
            }

            if execution_result.was_executed() {
                // Signature count must be accumulated only if the transaction
                // is executed, otherwise a mismatched count between banking and
                // replay could occur
                signature_count += u64::from(tx.message().header().num_required_signatures);
                executed_transactions_count += 1;
            }

            match execution_result.flattened_result() {
                Ok(()) => {
                    if !is_vote {
                        executed_non_vote_transactions_count += 1;
                    }
                    executed_with_successful_result_count += 1;
                }
                Err(err) => {
                    if *err_count == 0 {
                        debug!("tx error: {:?} {:?}", err, tx);
                    }
                    *err_count += 1;
                }
            }
        }
        collect_logs_time.stop();
        timings
            .saturating_add_in_place(ExecuteTimingType::CollectLogsUs, collect_logs_time.as_us());

        if *err_count > 0 {
            debug!(
                "{} errors of {} txs",
                *err_count,
                *err_count + executed_with_successful_result_count
            );
        }

        LoadAndExecuteTransactionsOutput {
            loaded_transactions: sanitized_output.loaded_transactions,
            execution_results: sanitized_output.execution_results,
            retryable_transaction_indexes,
            executed_transactions_count,
            executed_non_vote_transactions_count,
            executed_with_successful_result_count,
            signature_count,
            error_counters,
        }
    }
}

#[cfg(test)]
mod test {
    use solana_sdk::clock::MAX_PROCESSING_AGE;

    use super::*;

    pub fn init_logger() {
        let _ = env_logger::builder().is_test(true).try_init();
    }

    #[test]
    fn test_bank_empty_transactions() {
        init_logger();

        let bank = Bank::default();
        let txs = vec![];

        let batch = bank.prepare_sanitized_batch(&txs);
        let mut timings = ExecuteTimings::default();
        let res = bank.load_and_execute_transactions(
            &batch,
            MAX_PROCESSING_AGE,
            Default::default(),
            &mut timings,
            None,
            None,
        );
        eprintln!("{:#?}", res);
    }
}
