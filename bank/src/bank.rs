// FIXME: once we worked this out
#![allow(dead_code)]
#![allow(unused_variables)]

use std::{
    borrow::Cow,
    collections::HashSet,
    sync::{Arc, RwLock},
};

use crate::{bank_rc::BankRc, transaction_batch::TransactionBatch};
use log::debug;
use solana_accounts_db::{
    accounts::Accounts, accounts_db::AccountsDb, ancestors::Ancestors,
    blockhash_queue::BlockhashQueue,
};
use solana_measure::measure::Measure;
use solana_program_runtime::{
    loaded_programs::{BlockRelation, ForkGraph, LoadedPrograms},
    timings::{ExecuteTimingType, ExecuteTimings},
};
use solana_sdk::{
    account::AccountSharedData,
    clock::{Epoch, Slot},
    epoch_schedule::EpochSchedule,
    feature_set::FeatureSet,
    fee::FeeStructure,
    hash::Hash,
    nonce_info::NoncePartial,
    pubkey::Pubkey,
    rent_collector::RentCollector,
    transaction::{Result, SanitizedTransaction, MAX_TX_ACCOUNT_LOCKS},
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

impl Default for Bank {
    fn default() -> Self {
        // NOTE: we plan to only have one bank which is created when the validator starts up
        let slot = 0;
        let epoch = 0;

        let epoch_schedule = EpochSchedule::default();
        let fee_structure = FeeStructure::default();
        let runtime_config = Arc::new(RuntimeConfig::default());
        let loaded_programs_cache = {
            // TODO: not sure how this is setup more proper in the original implementation
            // since there we don't call `set_fork_graph` directly from the bank
            // Also we don't need bank forks in our initial implementation
            let mut loaded_programs = LoadedPrograms::new(slot, epoch);
            let simple_fork_graph = Arc::<RwLock<SimpleForkGraph>>::default();
            loaded_programs.set_fork_graph(simple_fork_graph);

            Arc::new(RwLock::new(loaded_programs))
        };

        let transaction_processor = TransactionBatchProcessor::new(
            slot,
            epoch,
            epoch_schedule.clone(),
            fee_structure.clone(),
            runtime_config.clone(),
            loaded_programs_cache.clone(),
        );

        // FIXME: @@@ cannot use `AccountsDb::default_for_tests()` here
        let accounts_db = Arc::new(AccountsDb::default_for_tests());
        let accounts = Accounts::new(accounts_db);

        // NOTE: these are configured via add_builtin `bank.rs:7045` and remove_builtin `bank.rs:7057`
        let builtin_programs = HashSet::new();

        let feature_set = Arc::<FeatureSet>::default();
        let rent_collector = RentCollector::default();
        let blockhash_queue = RwLock::<BlockhashQueue>::default();
        let ancestors = Ancestors::default();

        Self {
            rc: BankRc::new(accounts, slot),
            slot,
            epoch,
            epoch_schedule,
            fee_structure,
            runtime_config,
            builtin_programs,
            loaded_programs_cache,
            transaction_processor,
            // For TransactionProcessingCallback
            feature_set,
            rent_collector,
            blockhash_queue,
            ancestors,
        }
    }
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
    ) {
        let sanitized_txs = batch.sanitized_transactions();
        debug!("processing transactions: {}", sanitized_txs.len());

        let mut error_counters = TransactionErrorMetrics::default();

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

        debug!(
            "execution results {:#?}",
            sanitized_output.execution_results
        );
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
    fn test_bank() {
        init_logger();

        let bank = Bank::default();
        let txs = vec![];

        let batch = bank.prepare_sanitized_batch(&txs);
        let mut timings = ExecuteTimings::default();
        bank.load_and_execute_transactions(
            &batch,
            MAX_PROCESSING_AGE,
            Default::default(),
            &mut timings,
            None,
            None,
        );
    }
}
