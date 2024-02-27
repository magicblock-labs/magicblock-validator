// FIXME: once we worked this out
#![allow(dead_code)]
#![allow(unused_variables)]

use std::{
    borrow::Cow,
    collections::HashSet,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering},
        Arc, LockResult, RwLock, RwLockReadGuard, RwLockWriteGuard,
    },
};

use crate::{
    bank_helpers::{
        calculate_data_size_delta, get_epoch_secs, inherit_specially_retained_account_fields,
    },
    transaction_logs::{
        TransactionLogCollector, TransactionLogCollectorConfig, TransactionLogCollectorFilter,
        TransactionLogInfo,
    },
    transaction_results::LoadAndExecuteTransactionsOutput,
};
use crate::{
    bank_rc::BankRc,
    builtins::{BuiltinPrototype, BUILTINS},
    consts::LAMPORTS_PER_SIGNATURE,
    status_cache::StatusCache,
    transaction_batch::TransactionBatch,
    transaction_results::{TransactionBalances, TransactionBalancesSet},
};
use log::{debug, info, trace};
use solana_accounts_db::{
    accounts::{Accounts, TransactionLoadResult},
    accounts_db::{AccountShrinkThreshold, AccountsDb, AccountsDbConfig},
    accounts_index::{AccountSecondaryIndexes, ZeroLamport},
    accounts_update_notifier_interface::AccountsUpdateNotifier,
    ancestors::Ancestors,
    blockhash_queue::BlockhashQueue,
    storable_accounts::StorableAccounts,
    transaction_results::{
        TransactionExecutionDetails, TransactionExecutionResult, TransactionResults,
    },
};
use solana_bpf_loader_program::syscalls::create_program_runtime_environment_v1;
use solana_cost_model::cost_tracker::CostTracker;
use solana_loader_v4_program::create_program_runtime_environment_v2;
use solana_measure::measure::Measure;
use solana_program_runtime::{
    compute_budget_processor::process_compute_budget_instructions,
    loaded_programs::{BlockRelation, ForkGraph, LoadedProgram, LoadedProgramType, LoadedPrograms},
    timings::{ExecuteTimingType, ExecuteTimings},
};
use solana_sdk::{
    account::{
        create_account_shared_data_with_fields as create_account, create_executable_meta,
        from_account, Account, AccountSharedData, ReadableAccount, WritableAccount,
    },
    clock::{Epoch, Slot, UnixTimestamp, MAX_PROCESSING_AGE},
    epoch_schedule::EpochSchedule,
    feature,
    feature_set::{self, include_loaded_accounts_data_size_in_fee_calculation, FeatureSet},
    fee::FeeStructure,
    fee_calculator::FeeRateGovernor,
    genesis_config::GenesisConfig,
    hash::Hash,
    message::SanitizedMessage,
    native_loader,
    nonce::{self, state::DurableNonce, NONCED_TX_MARKER_IX_INDEX},
    nonce_account,
    nonce_info::{NonceInfo, NoncePartial},
    packet::PACKET_DATA_SIZE,
    precompiles::get_precompiles,
    pubkey::Pubkey,
    recent_blockhashes_account,
    rent_collector::RentCollector,
    rent_debits::RentDebits,
    saturating_add_assign,
    signature::Signature,
    sysvar::{self, last_restart_slot::LastRestartSlot},
    transaction::{
        Result, SanitizedTransaction, TransactionError, TransactionVerificationMode,
        VersionedTransaction, MAX_TX_ACCOUNT_LOCKS,
    },
    transaction_context::TransactionAccount,
};
use solana_svm::{account_loader::TransactionCheckResult, account_overrides::AccountOverrides};
use solana_svm::{runtime_config::RuntimeConfig, transaction_processor::TransactionBatchProcessor};
use solana_svm::{
    transaction_error_metrics::TransactionErrorMetrics,
    transaction_processor::TransactionProcessingCallback,
};
use solana_system_program::{get_system_account_kind, SystemAccountKind};

pub type BankStatusCache = StatusCache<Result<()>>;

pub struct CommitTransactionCounts {
    pub committed_transactions_count: u64,
    pub committed_non_vote_transactions_count: u64,
    pub committed_with_failure_result_count: u64,
    pub signature_count: u64,
}

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

    /// A boolean reflecting whether any entries were recorded into the PoH
    /// stream for the slot == self.slot
    is_delta: AtomicBool,

    builtin_programs: HashSet<Pubkey>,

    pub loaded_programs_cache: Arc<RwLock<LoadedPrograms<SimpleForkGraph>>>,

    pub(super) transaction_processor: TransactionBatchProcessor<SimpleForkGraph>,

    // Global configuration for how transaction logs should be collected across all banks
    pub transaction_log_collector_config: Arc<RwLock<TransactionLogCollectorConfig>>,

    // Logs from transactions that this Bank executed collected according to the criteria in
    // `transaction_log_collector_config`
    pub transaction_log_collector: Arc<RwLock<TransactionLogCollector>>,

    transaction_debug_keys: Option<Arc<HashSet<Pubkey>>>,

    /// A cache of signature statuses
    pub status_cache: Arc<RwLock<BankStatusCache>>,

    // -----------------
    // Counters
    // -----------------
    /// The number of transactions processed without error
    transaction_count: AtomicU64,

    /// The number of non-vote transactions processed without error since the most recent boot from
    /// snapshot or genesis. This value is not shared though the network, nor retained within
    /// snapshots, but is preserved in `Bank::new_from_parent`.
    non_vote_transaction_count_since_restart: AtomicU64,

    /// The number of transaction errors in this slot
    transaction_error_count: AtomicU64,

    /// The number of transaction entries in this slot
    transaction_entries_count: AtomicU64,

    /// The max number of transaction in an entry in this slot
    transactions_per_entry_max: AtomicU64,

    /// The change to accounts data size in this Bank, due on-chain events (i.e. transactions)
    accounts_data_size_delta_on_chain: AtomicI64,

    /// The change to accounts data size in this Bank, due to off-chain events (i.e. when adding a program account)
    accounts_data_size_delta_off_chain: AtomicI64,

    /// The number of signatures from valid transactions in this slot
    signature_count: AtomicU64,

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

    // -----------------
    // Cost
    // -----------------
    cost_tracker: RwLock<CostTracker>,
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
    pub enable_cpi_recording: bool,
    pub enable_log_recording: bool,
    pub enable_return_data_recording: bool,
}

impl TransactionExecutionRecordingOpts {
    pub fn recording_logs() -> Self {
        Self {
            enable_cpi_recording: false,
            enable_log_recording: true,
            enable_return_data_recording: false,
        }
    }

    pub fn recording_all() -> Self {
        Self {
            enable_cpi_recording: true,
            enable_log_recording: true,
            enable_return_data_recording: true,
        }
    }
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

        bank.finish_init(
            genesis_config,
            additional_builtins,
            debug_do_not_add_builtins,
        );

        // NOTE: leaving out stake history sysvar setup

        // For more info about sysvars see ../../docs/sysvars.md

        // We don't really have epochs so we use the validator start time
        bank.update_clock(genesis_config.creation_time);
        bank.update_rent();
        bank.update_fees();
        bank.update_epoch_schedule();
        bank.update_last_restart_slot();

        // NOTE: recent_blockhashes sysvar is deprecated and even if we add it to the sysvar
        //       cache requesting it from inside a transaction fails with `UnsupportedSysvar` error
        //       due to not supporting `Sysvar::get`.
        //       From sysvar/recent_blockhashes.rs: [`RecentBlockhashes`] does not implement [`Sysvar::get`].
        bank.update_recent_blockhashes();

        // NOTE: the below sets those sysvars once and thus they stay the same for the lifetime of the bank
        // in our case we'd need to find a way to update at least the clock more regularly and set
        // it via bank.transaction_processor.sysvar_cache.write().unwrap().set_clock(), etc.
        bank.fill_missing_sysvar_cache_entries();

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
            is_delta: AtomicBool::default(),
            builtin_programs: HashSet::<Pubkey>::default(),
            runtime_config: Arc::<RuntimeConfig>::default(),
            transaction_debug_keys: Option::<Arc<HashSet<Pubkey>>>::default(),
            transaction_log_collector_config: Arc::<RwLock<TransactionLogCollectorConfig>>::default(
            ),
            transaction_log_collector: Arc::<RwLock<TransactionLogCollector>>::default(),
            fee_structure: FeeStructure::default(),
            loaded_programs_cache,
            transaction_processor: TransactionBatchProcessor::default(),
            status_cache: Arc::<RwLock<BankStatusCache>>::default(),

            // Counters
            transaction_count: AtomicU64::default(),
            non_vote_transaction_count_since_restart: AtomicU64::default(),
            transaction_error_count: AtomicU64::default(),
            transaction_entries_count: AtomicU64::default(),
            transactions_per_entry_max: AtomicU64::default(),
            accounts_data_size_delta_on_chain: AtomicI64::default(),
            accounts_data_size_delta_off_chain: AtomicI64::default(),
            signature_count: AtomicU64::default(),

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

            // Cost
            cost_tracker: RwLock::<CostTracker>::default(),
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
    // Init
    // -----------------
    fn finish_init(
        &mut self,
        genesis_config: &GenesisConfig,
        additional_builtins: Option<&[BuiltinPrototype]>,
        debug_do_not_add_builtins: bool,
    ) {
        // NOTE: leaving out `rewards_pool_pubkeys` initialization

        self.apply_feature_activations(debug_do_not_add_builtins);

        if !debug_do_not_add_builtins {
            for builtin in BUILTINS
                .iter()
                .chain(additional_builtins.unwrap_or(&[]).iter())
            {
                if builtin.feature_id.is_none() {
                    self.add_builtin(
                        builtin.program_id,
                        builtin.name.to_string(),
                        LoadedProgram::new_builtin(0, builtin.name.len(), builtin.entrypoint),
                    );
                }
            }
            for precompile in get_precompiles() {
                if precompile.feature.is_none() {
                    self.add_precompile(&precompile.program_id);
                }
            }
        }

        let mut loaded_programs_cache = self.loaded_programs_cache.write().unwrap();
        loaded_programs_cache.latest_root_slot = self.slot();
        loaded_programs_cache.latest_root_epoch = self.epoch();
        loaded_programs_cache.environments.program_runtime_v1 = Arc::new(
            create_program_runtime_environment_v1(
                &self.feature_set,
                &self.runtime_config.compute_budget.unwrap_or_default(),
                false, /* deployment */
                false, /* debugging_features */
            )
            .unwrap(),
        );
        loaded_programs_cache.environments.program_runtime_v2 =
            Arc::new(create_program_runtime_environment_v2(
                &self.runtime_config.compute_budget.unwrap_or_default(),
                false, /* debugging_features */
            ));
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

        // NOTE: these accounts can include feature activation accounts which need to be
        // present in order to properly activate a feature
        // If not then activating all features results in a panic when executing a transaction
        for (pubkey, account) in genesis_config.accounts.iter() {
            assert!(
                self.get_account(pubkey).is_none(),
                "{pubkey} repeated in genesis config"
            );
            self.store_account(pubkey, account);
            self.capitalization
                .fetch_add(account.lamports(), Ordering::Relaxed);
            self.accounts_data_size_initial += account.data().len() as u64;
        }

        debug!("set blockhash {:?}", genesis_config.hash());
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
        for (name, program_id) in &genesis_config.native_instruction_processors {
            self.add_builtin_account(name, program_id, false);
        }
    }

    // -----------------
    // Slot and Epoch
    // -----------------
    pub fn slot(&self) -> Slot {
        self.slot
    }

    pub fn advance_slot(&mut self) {
        self.slot += 1;
        self.transaction_processor = TransactionBatchProcessor::new(
            self.slot,
            self.epoch,
            self.epoch_schedule.clone(),
            self.fee_structure.clone(),
            self.runtime_config.clone(),
            self.loaded_programs_cache.clone(),
        );

        self.fill_missing_sysvar_cache_entries();
    }

    pub fn epoch(&self) -> Epoch {
        self.epoch
    }

    pub fn epoch_schedule(&self) -> &EpochSchedule {
        &self.epoch_schedule
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
    pub fn get_account(&self, pubkey: &Pubkey) -> Option<AccountSharedData> {
        self.get_account_modified_slot(pubkey)
            .map(|(acc, _slot)| acc)
    }

    pub fn get_account_modified_slot(&self, pubkey: &Pubkey) -> Option<(AccountSharedData, Slot)> {
        self.load_slow(&self.ancestors, pubkey)
    }

    pub fn get_account_with_fixed_root(&self, pubkey: &Pubkey) -> Option<AccountSharedData> {
        self.get_account_modified_slot_with_fixed_root(pubkey)
            .map(|(acc, _slot)| acc)
    }

    pub fn get_account_modified_slot_with_fixed_root(
        &self,
        pubkey: &Pubkey,
    ) -> Option<(AccountSharedData, Slot)> {
        self.load_slow_with_fixed_root(&self.ancestors, pubkey)
    }

    fn load_slow(
        &self,
        ancestors: &Ancestors,
        pubkey: &Pubkey,
    ) -> Option<(AccountSharedData, Slot)> {
        self.rc.accounts.load_without_fixed_root(ancestors, pubkey)
    }

    fn load_slow_with_fixed_root(
        &self,
        ancestors: &Ancestors,
        pubkey: &Pubkey,
    ) -> Option<(AccountSharedData, Slot)> {
        self.rc.accounts.load_with_fixed_root(ancestors, pubkey)
    }

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

    /// Technically this issues (or even burns!) new lamports,
    /// so be extra careful for its usage
    fn store_account_and_update_capitalization(
        &self,
        pubkey: &Pubkey,
        new_account: &AccountSharedData,
    ) {
        let old_account_data_size =
            if let Some(old_account) = self.get_account_with_fixed_root(pubkey) {
                match new_account.lamports().cmp(&old_account.lamports()) {
                    std::cmp::Ordering::Greater => {
                        let increased = new_account.lamports() - old_account.lamports();
                        trace!(
                            "store_account_and_update_capitalization: increased: {} {}",
                            pubkey,
                            increased
                        );
                        self.capitalization.fetch_add(increased, Ordering::Relaxed);
                    }
                    std::cmp::Ordering::Less => {
                        let decreased = old_account.lamports() - new_account.lamports();
                        trace!(
                            "store_account_and_update_capitalization: decreased: {} {}",
                            pubkey,
                            decreased
                        );
                        self.capitalization.fetch_sub(decreased, Ordering::Relaxed);
                    }
                    std::cmp::Ordering::Equal => {}
                }
                old_account.data().len()
            } else {
                trace!(
                    "store_account_and_update_capitalization: created: {} {}",
                    pubkey,
                    new_account.lamports()
                );
                self.capitalization
                    .fetch_add(new_account.lamports(), Ordering::Relaxed);
                0
            };

        self.store_account(pubkey, new_account);
        self.calculate_and_update_accounts_data_size_delta_off_chain(
            old_account_data_size,
            new_account.data().len(),
        );
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
    // Balances
    // -----------------
    pub fn collect_balances(&self, batch: &TransactionBatch) -> TransactionBalances {
        let mut balances: TransactionBalances = vec![];
        for transaction in batch.sanitized_transactions() {
            let mut transaction_balances: Vec<u64> = vec![];
            for account_key in transaction.message().account_keys().iter() {
                transaction_balances.push(self.get_balance(account_key));
            }
            balances.push(transaction_balances);
        }
        balances
    }

    /// Each program would need to be able to introspect its own state
    /// this is hard-coded to the Budget language
    pub fn get_balance(&self, pubkey: &Pubkey) -> u64 {
        self.get_account(pubkey)
            .map(|x| Self::read_balance(&x))
            .unwrap_or(0)
    }

    pub fn read_balance(account: &AccountSharedData) -> u64 {
        account.lamports()
    }

    // -----------------
    // SysVars
    // -----------------
    pub fn clock(&self) -> sysvar::clock::Clock {
        from_account(&self.get_account(&sysvar::clock::id()).unwrap_or_default())
            .unwrap_or_default()
    }

    fn update_clock(&self, epoch_start_timestamp: UnixTimestamp) {
        // NOTE: the Solana validator determines time with a much more complex logic
        // - slot == 0: genesis creation time + number of slots * ns_per_slot to seconds
        // - slot > 0 : epoch start time + number of slots to get a timestamp estimate with max
        //              allowable drift
        // Different timestamp votes are then considered, taking stake into account and the median
        // is used as the final value.
        // Possibly for that reason the solana UnixTimestamp is an i64 in order to make those
        // calculations easier.
        // This makes sense since otherwise the hosting platform could manipulate the time assumed
        // by the validator.
        let unix_timestamp = i64::try_from(get_epoch_secs()).expect("get_epoch_secs overflow");

        let clock = sysvar::clock::Clock {
            slot: self.slot,
            epoch_start_timestamp,
            epoch: self.epoch_schedule().get_epoch(self.slot),
            leader_schedule_epoch: self.epoch_schedule().get_leader_schedule_epoch(self.slot),
            unix_timestamp,
        };
        self.update_sysvar_account(&sysvar::clock::id(), |account| {
            create_account(&clock, inherit_specially_retained_account_fields(account))
        });
    }

    fn update_rent(&self) {
        self.update_sysvar_account(&sysvar::rent::id(), |account| {
            create_account(
                &self.rent_collector.rent,
                inherit_specially_retained_account_fields(account),
            )
        });
    }

    #[allow(deprecated)]
    fn update_fees(&self) {
        if !self
            .feature_set
            .is_active(&feature_set::disable_fees_sysvar::id())
        {
            self.update_sysvar_account(&sysvar::fees::id(), |account| {
                create_account(
                    &sysvar::fees::Fees::new(&self.fee_rate_governor.create_fee_calculator()),
                    inherit_specially_retained_account_fields(account),
                )
            });
        }
    }

    fn update_epoch_schedule(&self) {
        self.update_sysvar_account(&sysvar::epoch_schedule::id(), |account| {
            create_account(
                self.epoch_schedule(),
                inherit_specially_retained_account_fields(account),
            )
        });
    }

    pub fn update_last_restart_slot(&self) {
        let feature_flag = self
            .feature_set
            .is_active(&feature_set::last_restart_slot_sysvar::id());

        if feature_flag {
            // First, see what the currently stored last restart slot is. This
            // account may not exist yet if the feature was just activated.
            let current_last_restart_slot = self
                .get_account(&sysvar::last_restart_slot::id())
                .and_then(|account| {
                    let lrs: Option<LastRestartSlot> = from_account(&account);
                    lrs
                })
                .map(|account| account.last_restart_slot);

            let last_restart_slot = 0;
            // NOTE: removed querying hard forks here

            // Only need to write if the last restart has changed
            if current_last_restart_slot != Some(last_restart_slot) {
                self.update_sysvar_account(&sysvar::last_restart_slot::id(), |account| {
                    create_account(
                        &LastRestartSlot { last_restart_slot },
                        inherit_specially_retained_account_fields(account),
                    )
                });
            }
        }
    }

    pub fn update_recent_blockhashes(&self) {
        let blockhash_queue = self.blockhash_queue.read().unwrap();
        self.update_recent_blockhashes_locked(&blockhash_queue);
    }

    fn update_recent_blockhashes_locked(&self, locked_blockhash_queue: &BlockhashQueue) {
        #[allow(deprecated)]
        self.update_sysvar_account(&sysvar::recent_blockhashes::id(), |account| {
            let recent_blockhash_iter = locked_blockhash_queue.get_recent_blockhashes();
            recent_blockhashes_account::create_account_with_data_and_fields(
                recent_blockhash_iter,
                inherit_specially_retained_account_fields(account),
            )
        });
    }

    fn update_sysvar_account<F>(&self, pubkey: &Pubkey, updater: F)
    where
        F: Fn(&Option<AccountSharedData>) -> AccountSharedData,
    {
        let old_account = self.get_account_with_fixed_root(pubkey);
        let mut new_account = updater(&old_account);

        // When new sysvar comes into existence (with RENT_UNADJUSTED_INITIAL_BALANCE lamports),
        // this code ensures that the sysvar's balance is adjusted to be rent-exempt.
        //
        // More generally, this code always re-calculates for possible sysvar data size change,
        // although there is no such sysvars currently.
        self.adjust_sysvar_balance_for_rent(&mut new_account);
        self.store_account_and_update_capitalization(pubkey, &new_account);
    }

    fn adjust_sysvar_balance_for_rent(&self, account: &mut AccountSharedData) {
        account.set_lamports(
            self.get_minimum_balance_for_rent_exemption(account.data().len())
                .max(account.lamports()),
        );
    }

    pub fn get_minimum_balance_for_rent_exemption(&self, data_len: usize) -> u64 {
        self.rent_collector.rent.minimum_balance(data_len).max(1)
    }

    // -----------------
    // Features
    // -----------------
    // In Solana this is called from snapshot restore AND for each epoch boundary
    // The entire code path herein must be idempotent
    // In our case only during finish_init when the bank is created
    fn apply_feature_activations(&mut self, debug_do_not_add_builtins: bool) {
        let feature_set = self.compute_active_feature_set();
        // NOTE: at this point we have only inactive features
        self.feature_set = Arc::new(feature_set);
    }

    /// Compute the active feature set based on the current bank state,
    /// and return it together with the set of newly activated features (we don't).
    fn compute_active_feature_set(&self) -> FeatureSet {
        // NOTE: took out the `pending` features since we don't support new feature activations
        // which in Solana only are used when we create a bank from a parent bank
        let mut active = self.feature_set.active.clone();
        let mut inactive = HashSet::new();
        let slot = self.slot();

        for feature_id in &self.feature_set.inactive {
            let mut activated = None;
            if let Some(account) = self.get_account_with_fixed_root(feature_id) {
                if let Some(feature) = feature::from_account(&account) {
                    match feature.activated_at {
                        Some(activation_slot) if slot >= activation_slot => {
                            // Feature has been activated already
                            activated = Some(activation_slot);
                        }
                        _ => {}
                    }
                }
            }
            if let Some(slot) = activated {
                active.insert(*feature_id, slot);
            } else {
                inactive.insert(*feature_id);
            }
        }

        FeatureSet { active, inactive }
    }

    // -----------------
    // Builtin Program Accounts
    // -----------------

    /// Add a built-in program
    pub fn add_builtin(&mut self, program_id: Pubkey, name: String, builtin: LoadedProgram) {
        debug!("Adding program {} under {:?}", name, program_id);
        self.add_builtin_account(name.as_str(), &program_id, false);
        self.builtin_programs.insert(program_id);
        self.loaded_programs_cache
            .write()
            .unwrap()
            .assign_program(program_id, Arc::new(builtin));
    }

    /// Add a builtin program account
    pub fn add_builtin_account(&self, name: &str, program_id: &Pubkey, must_replace: bool) {
        let existing_genuine_program =
            self.get_account_with_fixed_root(program_id)
                .and_then(|account| {
                    // it's very unlikely to be squatted at program_id as non-system account because of burden to
                    // find victim's pubkey/hash. So, when account.owner is indeed native_loader's, it's
                    // safe to assume it's a genuine program.
                    if native_loader::check_id(account.owner()) {
                        Some(account)
                    } else {
                        // malicious account is pre-occupying at program_id
                        self.burn_and_purge_account(program_id, account);
                        None
                    }
                });

        if must_replace {
            // updating builtin program
            match &existing_genuine_program {
                None => panic!(
                    "There is no account to replace with builtin program ({name}, {program_id})."
                ),
                Some(account) => {
                    if *name == String::from_utf8_lossy(account.data()) {
                        // The existing account is well formed
                        return;
                    }
                }
            }
        } else {
            // introducing builtin program
            if existing_genuine_program.is_some() {
                // The existing account is sufficient
                return;
            }
        }

        assert!(
            !self.freeze_started(),
            "Can't change frozen bank by adding not-existing new builtin program ({name}, {program_id}). \
            Maybe, inconsistent program activation is detected on snapshot restore?"
        );

        // Add a bogus executable builtin account, which will be loaded and ignored.
        let account = native_loader::create_loadable_account_with_fields(
            name,
            inherit_specially_retained_account_fields(&existing_genuine_program),
        );
        self.store_account_and_update_capitalization(program_id, &account);
    }

    // Looks like this is only used in tests since add_precompiled_account_with_owner is as well
    // However `finish_init` is calling this method, so we keep it here
    pub fn add_precompile(&mut self, program_id: &Pubkey) {
        debug!("Adding precompiled program {}", program_id);
        self.add_precompiled_account(program_id);
    }

    /// Add a precompiled program account
    pub fn add_precompiled_account(&self, program_id: &Pubkey) {
        self.add_precompiled_account_with_owner(program_id, native_loader::id())
    }

    // Used by tests to simulate clusters with precompiles that aren't owned by the native loader
    fn add_precompiled_account_with_owner(&self, program_id: &Pubkey, owner: Pubkey) {
        if let Some(account) = self.get_account_with_fixed_root(program_id) {
            if account.executable() {
                return;
            }
            // malicious account is pre-occupying at program_id
            self.burn_and_purge_account(program_id, account);
        };

        assert!(
            !self.freeze_started(),
            "Can't change frozen bank by adding not-existing new precompiled program ({program_id}). \
                Maybe, inconsistent program activation is detected on snapshot restore?"
        );

        // Add a bogus executable account, which will be loaded and ignored.
        let (lamports, rent_epoch) = inherit_specially_retained_account_fields(&None);

        // Mock account_data with executable_meta so that the account is executable.
        let account_data = create_executable_meta(&owner);
        let account = AccountSharedData::from(Account {
            lamports,
            owner,
            data: account_data.to_vec(),
            executable: true,
            rent_epoch,
        });
        self.store_account_and_update_capitalization(program_id, &account);
    }

    fn burn_and_purge_account(&self, program_id: &Pubkey, mut account: AccountSharedData) {
        let old_data_size = account.data().len();
        self.capitalization
            .fetch_sub(account.lamports(), Ordering::Relaxed);
        // Both resetting account balance to 0 and zeroing the account data
        // is needed to really purge from AccountsDb and flush the Stakes cache
        account.set_lamports(0);
        account.data_as_mut_slice().fill(0);
        self.store_account(program_id, &account);
        self.calculate_and_update_accounts_data_size_delta_off_chain(old_data_size, 0);
    }

    /// Remove a built-in instruction processor
    pub fn remove_builtin(&mut self, program_id: Pubkey, name: String) {
        debug!("Removing program {}", program_id);
        // Don't remove the account since the bank expects the account state to
        // be idempotent
        self.add_builtin(
            program_id,
            name,
            LoadedProgram::new_tombstone(self.slot, LoadedProgramType::Closed),
        );
        debug!("Removed program {}", program_id);
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

    /// Prepare a locked transaction batch from a list of sanitized transactions, and their cost
    /// limited packing status
    pub fn prepare_sanitized_batch_with_results<'a, 'b>(
        &'a self,
        transactions: &'b [SanitizedTransaction],
        transaction_results: impl Iterator<Item = Result<()>>,
    ) -> TransactionBatch<'a, 'b> {
        // this lock_results could be: Ok, AccountInUse, WouldExceedBlockMaxLimit or WouldExceedAccountMaxLimit
        let tx_account_lock_limit = self.get_transaction_account_lock_limit();
        let lock_results = self.rc.accounts.lock_accounts_with_results(
            transactions.iter(),
            transaction_results,
            tx_account_lock_limit,
        );
        TransactionBatch::new(lock_results, self, Cow::Borrowed(transactions))
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
        // FIXME: skipping check_transactions runtime/src/bank.rs: 4505 (which is mostly tx age related)
        sanitized_txs
            .iter()
            .map(|_| {
                // NOTE: if we don't provide some lamports per signature we then the
                // TransactionBatchProcessor::filter_executable_program_accounts will
                // add a BlockhashNotFound TransactionCheckResult which causes the transaction
                // to not execute
                let res: TransactionCheckResult =
                    (Ok(()), None::<NoncePartial>, Some(LAMPORTS_PER_SIGNATURE));
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

    /// Process a batch of transactions.
    #[must_use]
    pub fn load_execute_and_commit_transactions(
        &self,
        batch: &TransactionBatch,
        max_age: usize,
        collect_balances: bool,
        recording_opts: TransactionExecutionRecordingOpts,
        timings: &mut ExecuteTimings,
        log_messages_bytes_limit: Option<usize>,
    ) -> (TransactionResults, TransactionBalancesSet) {
        let pre_balances = if collect_balances {
            self.collect_balances(batch)
        } else {
            vec![]
        };

        let LoadAndExecuteTransactionsOutput {
            mut loaded_transactions,
            execution_results,
            executed_transactions_count,
            executed_non_vote_transactions_count,
            executed_with_successful_result_count,
            signature_count,
            ..
        } = self.load_and_execute_transactions(
            batch,
            max_age,
            recording_opts,
            timings,
            None,
            log_messages_bytes_limit,
        );

        let (last_blockhash, lamports_per_signature) =
            self.last_blockhash_and_lamports_per_signature();
        let results = self.commit_transactions(
            batch.sanitized_transactions(),
            &mut loaded_transactions,
            execution_results,
            last_blockhash,
            lamports_per_signature,
            CommitTransactionCounts {
                committed_transactions_count: executed_transactions_count as u64,
                committed_non_vote_transactions_count: executed_non_vote_transactions_count as u64,
                committed_with_failure_result_count: executed_transactions_count
                    .saturating_sub(executed_with_successful_result_count)
                    as u64,
                signature_count,
            },
            timings,
        );
        let post_balances = if collect_balances {
            self.collect_balances(batch)
        } else {
            vec![]
        };
        (
            results,
            TransactionBalancesSet::new(pre_balances, post_balances),
        )
    }

    #[must_use]
    pub(super) fn process_transaction_batch(&self, batch: &TransactionBatch) -> Vec<Result<()>> {
        self.load_execute_and_commit_transactions(
            batch,
            MAX_PROCESSING_AGE,
            false,
            Default::default(),
            &mut ExecuteTimings::default(),
            None,
        )
        .0
        .fee_collection_results
    }

    /// `committed_transactions_count` is the number of transactions out of `sanitized_txs`
    /// that was executed. Of those, `committed_transactions_count`,
    /// `committed_with_failure_result_count` is the number of executed transactions that returned
    /// a failure result.
    #[allow(clippy::too_many_arguments)]
    pub fn commit_transactions(
        &self,
        sanitized_txs: &[SanitizedTransaction],
        loaded_txs: &mut [TransactionLoadResult],
        execution_results: Vec<TransactionExecutionResult>,
        last_blockhash: Hash,
        lamports_per_signature: u64,
        counts: CommitTransactionCounts,
        timings: &mut ExecuteTimings,
    ) -> TransactionResults {
        assert!(
            !self.freeze_started(),
            "commit_transactions() working on a bank that is already frozen or is undergoing freezing!");

        let CommitTransactionCounts {
            committed_transactions_count,
            committed_non_vote_transactions_count,
            committed_with_failure_result_count,
            signature_count,
        } = counts;

        self.increment_transaction_count(committed_transactions_count);
        self.increment_non_vote_transaction_count_since_restart(
            committed_non_vote_transactions_count,
        );
        self.increment_signature_count(signature_count);

        if committed_with_failure_result_count > 0 {
            self.transaction_error_count
                .fetch_add(committed_with_failure_result_count, Ordering::Relaxed);
        }

        // Should be equivalent to checking `committed_transactions_count > 0`
        if execution_results.iter().any(|result| result.was_executed()) {
            self.is_delta.store(true, Ordering::Relaxed);
            self.transaction_entries_count
                .fetch_add(1, Ordering::Relaxed);
            self.transactions_per_entry_max
                .fetch_max(committed_transactions_count, Ordering::Relaxed);
        }

        let mut write_time = Measure::start("write_time");
        let durable_nonce = DurableNonce::from_blockhash(&last_blockhash);
        self.rc.accounts.store_cached(
            self.slot(),
            sanitized_txs,
            &execution_results,
            loaded_txs,
            &durable_nonce,
            lamports_per_signature,
        );
        let rent_debits = self.collect_rent(&execution_results, loaded_txs);

        let mut update_stakes_cache_time = Measure::start("update_stakes_cache_time");
        self.update_stakes_cache(sanitized_txs, &execution_results, loaded_txs);
        update_stakes_cache_time.stop();

        // once committed there is no way to unroll
        write_time.stop();
        debug!(
            "store: {}us txs_len={}",
            write_time.as_us(),
            sanitized_txs.len()
        );

        let mut store_executors_which_were_deployed_time =
            Measure::start("store_executors_which_were_deployed_time");
        for execution_result in &execution_results {
            if let TransactionExecutionResult::Executed {
                details,
                programs_modified_by_tx,
            } = execution_result
            {
                if details.status.is_ok() {
                    let mut cache = self.loaded_programs_cache.write().unwrap();
                    cache.merge(programs_modified_by_tx);
                }
            }
        }
        store_executors_which_were_deployed_time.stop();
        saturating_add_assign!(
            timings.execute_accessories.update_executors_us,
            store_executors_which_were_deployed_time.as_us()
        );

        let accounts_data_len_delta = execution_results
            .iter()
            .filter_map(TransactionExecutionResult::details)
            .filter_map(|details| {
                details
                    .status
                    .is_ok()
                    .then_some(details.accounts_data_len_delta)
            })
            .sum();
        self.update_accounts_data_size_delta_on_chain(accounts_data_len_delta);

        timings.saturating_add_in_place(ExecuteTimingType::StoreUs, write_time.as_us());

        let mut update_transaction_statuses_time = Measure::start("update_transaction_statuses");
        self.update_transaction_statuses(sanitized_txs, &execution_results);
        let fee_collection_results =
            self.filter_program_errors_and_collect_fee(sanitized_txs, &execution_results);
        update_transaction_statuses_time.stop();
        timings.saturating_add_in_place(
            ExecuteTimingType::UpdateTransactionStatuses,
            update_transaction_statuses_time.as_us(),
        );

        TransactionResults {
            fee_collection_results,
            execution_results,
            rent_debits,
        }
    }

    fn update_transaction_statuses(
        &self,
        sanitized_txs: &[SanitizedTransaction],
        execution_results: &[TransactionExecutionResult],
    ) {
        let mut status_cache = self.status_cache.write().unwrap();
        assert_eq!(sanitized_txs.len(), execution_results.len());
        for (tx, execution_result) in sanitized_txs.iter().zip(execution_results) {
            if let Some(details) = execution_result.details() {
                // Add the message hash to the status cache to ensure that this message
                // won't be processed again with a different signature.
                status_cache.insert(
                    tx.message().recent_blockhash(),
                    tx.message_hash(),
                    self.slot(),
                    details.status.clone(),
                );
                // Add the transaction signature to the status cache so that transaction status
                // can be queried by transaction signature over RPC. In the future, this should
                // only be added for API nodes because voting validators don't need to do this.
                status_cache.insert(
                    tx.message().recent_blockhash(),
                    tx.signature(),
                    self.slot(),
                    details.status.clone(),
                );
            }
        }
    }

    fn filter_program_errors_and_collect_fee(
        &self,
        txs: &[SanitizedTransaction],
        execution_results: &[TransactionExecutionResult],
    ) -> Vec<Result<()>> {
        let hash_queue = self.blockhash_queue.read().unwrap();
        let mut fees = 0;

        let results = txs
            .iter()
            .zip(execution_results)
            .map(|(tx, execution_result)| {
                let (execution_status, durable_nonce_fee) = match &execution_result {
                    TransactionExecutionResult::Executed { details, .. } => {
                        Ok((&details.status, details.durable_nonce_fee.as_ref()))
                    }
                    TransactionExecutionResult::NotExecuted(err) => Err(err.clone()),
                }?;

                let (lamports_per_signature, is_nonce) = durable_nonce_fee
                    .map(|durable_nonce_fee| durable_nonce_fee.lamports_per_signature())
                    .map(|maybe_lamports_per_signature| (maybe_lamports_per_signature, true))
                    .unwrap_or_else(|| {
                        (
                            hash_queue.get_lamports_per_signature(tx.message().recent_blockhash()),
                            false,
                        )
                    });

                let lamports_per_signature =
                    lamports_per_signature.ok_or(TransactionError::BlockhashNotFound)?;

                let fee = self.get_fee_for_message_with_lamports_per_signature(
                    tx.message(),
                    lamports_per_signature,
                );

                // In case of instruction error, even though no accounts
                // were stored we still need to charge the payer the
                // fee.
                //
                //...except nonce accounts, which already have their
                // post-load, fee deducted, pre-execute account state
                // stored
                if execution_status.is_err() && !is_nonce {
                    self.withdraw(tx.message().fee_payer(), fee)?;
                }

                fees += fee;
                Ok(())
            })
            .collect();

        // self.collector_fees.fetch_add(fees, Relaxed);
        results
    }

    // -----------------
    // Transaction Verification
    // -----------------
    pub fn verify_transaction(
        &self,
        tx: VersionedTransaction,
        verification_mode: TransactionVerificationMode,
    ) -> Result<SanitizedTransaction> {
        let sanitized_tx = {
            let size =
                bincode::serialized_size(&tx).map_err(|_| TransactionError::SanitizeFailure)?;
            if size > PACKET_DATA_SIZE as u64 {
                return Err(TransactionError::SanitizeFailure);
            }
            let message_hash = if verification_mode == TransactionVerificationMode::FullVerification
            {
                tx.verify_and_hash_message()?
            } else {
                tx.message.hash()
            };

            SanitizedTransaction::try_create(tx, message_hash, None, self)
        }?;

        if verification_mode == TransactionVerificationMode::HashAndVerifyPrecompiles
            || verification_mode == TransactionVerificationMode::FullVerification
        {
            sanitized_tx.verify_precompiles(&self.feature_set)?;
        }

        Ok(sanitized_tx)
    }

    pub fn fully_verify_transaction(
        &self,
        tx: VersionedTransaction,
    ) -> Result<SanitizedTransaction> {
        self.verify_transaction(tx, TransactionVerificationMode::FullVerification)
    }

    // -----------------
    // Fees
    // -----------------
    fn withdraw(&self, pubkey: &Pubkey, lamports: u64) -> Result<()> {
        match self.get_account_with_fixed_root(pubkey) {
            Some(mut account) => {
                let min_balance = match get_system_account_kind(&account) {
                    Some(SystemAccountKind::Nonce) => self
                        .rent_collector
                        .rent
                        .minimum_balance(nonce::State::size()),
                    _ => 0,
                };

                lamports
                    .checked_add(min_balance)
                    .filter(|required_balance| *required_balance <= account.lamports())
                    .ok_or(TransactionError::InsufficientFundsForFee)?;
                account
                    .checked_sub_lamports(lamports)
                    .map_err(|_| TransactionError::InsufficientFundsForFee)?;
                self.store_account(pubkey, &account);

                Ok(())
            }
            None => Err(TransactionError::AccountNotFound),
        }
    }

    pub fn get_lamports_per_signature(&self) -> u64 {
        self.fee_rate_governor.lamports_per_signature
    }

    pub fn get_fee_for_message(&self, message: &SanitizedMessage) -> Option<u64> {
        let lamports_per_signature = {
            let blockhash_queue = self.blockhash_queue.read().unwrap();
            blockhash_queue.get_lamports_per_signature(message.recent_blockhash())
        }
        .or_else(|| {
            self.check_message_for_nonce(message)
                .and_then(|(address, account)| {
                    NoncePartial::new(address, account).lamports_per_signature()
                })
        })?;
        Some(self.get_fee_for_message_with_lamports_per_signature(message, lamports_per_signature))
    }

    pub fn get_fee_for_message_with_lamports_per_signature(
        &self,
        message: &SanitizedMessage,
        lamports_per_signature: u64,
    ) -> u64 {
        self.fee_structure.calculate_fee(
            message,
            lamports_per_signature,
            &process_compute_budget_instructions(message.program_instructions_iter())
                .unwrap_or_default()
                .into(),
            self.feature_set
                .is_active(&include_loaded_accounts_data_size_in_fee_calculation::id()),
        )
    }

    // -----------------
    // Nonces
    // -----------------
    fn check_message_for_nonce(&self, message: &SanitizedMessage) -> Option<TransactionAccount> {
        let nonce_address = message.get_durable_nonce()?;
        let nonce_account = self.get_account_with_fixed_root(nonce_address)?;
        let nonce_data =
            nonce_account::verify_nonce_account(&nonce_account, message.recent_blockhash())?;

        let nonce_is_authorized = message
            .get_ix_signers(NONCED_TX_MARKER_IX_INDEX as usize)
            .any(|signer| signer == &nonce_data.authority);
        if !nonce_is_authorized {
            return None;
        }

        Some((*nonce_address, nonce_account))
    }

    // -----------------
    // Unsupported
    // -----------------
    fn collect_rent(
        &self,
        _execution_results: &[TransactionExecutionResult],
        _loaded_txs: &mut [TransactionLoadResult],
    ) -> Vec<RentDebits> {
        vec![]
    }

    fn update_stakes_cache(
        &self,
        _txs: &[SanitizedTransaction],
        _execution_results: &[TransactionExecutionResult],
        _loaded_txs: &[TransactionLoadResult],
    ) {
    }

    pub fn is_frozen(&self) -> bool {
        false
    }

    pub fn freeze_started(&self) -> bool {
        false
    }

    pub fn parent(&self) -> Option<Arc<Bank>> {
        None
    }
    // -----------------
    // Signature Status
    // -----------------
    pub fn get_signature_status_slot(&self, signature: &Signature) -> Option<(Slot, Result<()>)> {
        // TODO: this currently behaves as if we had multiple banks and/or forks
        // we should simplify this to get info from the current bank only
        let rcache = self.status_cache.read().unwrap();
        rcache.get_status_any_blockhash(signature, &self.ancestors)
    }

    pub fn get_signature_status(&self, signature: &Signature) -> Option<Result<()>> {
        self.get_signature_status_slot(signature).map(|v| v.1)
    }

    pub fn has_signature(&self, signature: &Signature) -> bool {
        self.get_signature_status_slot(signature).is_some()
    }

    // -----------------
    // Counters
    // -----------------
    /// Return the accumulated executed transaction count
    pub fn transaction_count(&self) -> u64 {
        self.transaction_count.load(Ordering::Relaxed)
    }

    /// Returns the number of non-vote transactions processed without error
    /// since the most recent boot from snapshot or genesis.
    /// This value is not shared though the network, nor retained
    /// within snapshots, but is preserved in `Bank::new_from_parent`.
    pub fn non_vote_transaction_count_since_restart(&self) -> u64 {
        self.non_vote_transaction_count_since_restart
            .load(Ordering::Relaxed)
    }

    /// Return the transaction count executed only in this bank
    pub fn executed_transaction_count(&self) -> u64 {
        self.transaction_count()
            .saturating_sub(self.parent().map_or(0, |parent| parent.transaction_count()))
    }

    pub fn transaction_error_count(&self) -> u64 {
        self.transaction_error_count.load(Ordering::Relaxed)
    }

    pub fn transaction_entries_count(&self) -> u64 {
        self.transaction_entries_count.load(Ordering::Relaxed)
    }

    pub fn transactions_per_entry_max(&self) -> u64 {
        self.transactions_per_entry_max.load(Ordering::Relaxed)
    }

    fn increment_transaction_count(&self, tx_count: u64) {
        self.transaction_count
            .fetch_add(tx_count, Ordering::Relaxed);
    }

    fn increment_non_vote_transaction_count_since_restart(&self, tx_count: u64) {
        self.non_vote_transaction_count_since_restart
            .fetch_add(tx_count, Ordering::Relaxed);
    }

    fn increment_signature_count(&self, signature_count: u64) {
        self.signature_count
            .fetch_add(signature_count, Ordering::Relaxed);
    }

    /// Update the accounts data size delta from on-chain events by adding `amount`.
    /// The arithmetic saturates.
    fn update_accounts_data_size_delta_on_chain(&self, amount: i64) {
        if amount == 0 {
            return;
        }

        self.accounts_data_size_delta_on_chain
            .fetch_update(
                Ordering::AcqRel,
                Ordering::Acquire,
                |accounts_data_size_delta_on_chain| {
                    Some(accounts_data_size_delta_on_chain.saturating_add(amount))
                },
            )
            // SAFETY: unwrap() is safe since our update fn always returns `Some`
            .unwrap();
    }

    /// Update the accounts data size delta from off-chain events by adding `amount`.
    /// The arithmetic saturates.
    fn update_accounts_data_size_delta_off_chain(&self, amount: i64) {
        if amount == 0 {
            return;
        }

        self.accounts_data_size_delta_off_chain
            .fetch_update(
                Ordering::AcqRel,
                Ordering::Acquire,
                |accounts_data_size_delta_off_chain| {
                    Some(accounts_data_size_delta_off_chain.saturating_add(amount))
                },
            )
            // SAFETY: unwrap() is safe since our update fn always returns `Some`
            .unwrap();
    }

    /// Calculate the data size delta and update the off-chain accounts data size delta
    fn calculate_and_update_accounts_data_size_delta_off_chain(
        &self,
        old_data_size: usize,
        new_data_size: usize,
    ) {
        let data_size_delta = calculate_data_size_delta(old_data_size, new_data_size);
        self.update_accounts_data_size_delta_off_chain(data_size_delta);
    }

    // -----------------
    // Accessors
    // -----------------
    pub fn read_cost_tracker(&self) -> LockResult<RwLockReadGuard<CostTracker>> {
        self.cost_tracker.read()
    }

    pub fn write_cost_tracker(&self) -> LockResult<RwLockWriteGuard<CostTracker>> {
        self.cost_tracker.write()
    }
}
