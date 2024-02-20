// FIXME: once we worked this out
#![allow(dead_code)]
#![allow(unused_variables)]

use std::{
    borrow::Cow,
    sync::{Arc, RwLock},
};

use crate::{bank_rc::BankRc, transaction_batch::TransactionBatch};
use log::debug;
use solana_accounts_db::{accounts::Accounts, accounts_db::AccountsDb};
use solana_program_runtime::loaded_programs::{BlockRelation, ForkGraph, LoadedPrograms};
use solana_sdk::{
    clock::{Epoch, Slot},
    epoch_schedule::EpochSchedule,
    fee::FeeStructure,
    transaction::{SanitizedTransaction, MAX_TX_ACCOUNT_LOCKS},
};
use solana_svm::{runtime_config::RuntimeConfig, transaction_processor::TransactionBatchProcessor};

// -----------------
// ForkGraph
// -----------------
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

    pub loaded_programs_cache: Arc<RwLock<LoadedPrograms<SimpleForkGraph>>>,

    transaction_processor: TransactionBatchProcessor<SimpleForkGraph>,
}

impl Default for Bank {
    fn default() -> Self {
        // NOTE: we plan to only have one bank which is created when the validator starts up
        let slot = 0;
        let epoch = 0;

        let epoch_schedule = EpochSchedule::default();
        let fee_structure = FeeStructure::default();
        let runtime_config = Arc::new(RuntimeConfig::default());
        let loaded_programs_cache = Arc::new(RwLock::new(LoadedPrograms::new(slot, epoch)));

        let transaction_processor = TransactionBatchProcessor::new(
            slot,
            epoch,
            epoch_schedule.clone(),
            fee_structure.clone(),
            runtime_config.clone(),
            loaded_programs_cache.clone(),
        );

        // TODO @@@: fix
        let accounts_db = Arc::new(AccountsDb::default_for_tests());
        let accounts = Accounts::new(accounts_db);

        Self {
            rc: BankRc::new(accounts, slot),
            slot,
            epoch,
            epoch_schedule,
            fee_structure,
            runtime_config,
            loaded_programs_cache,
            transaction_processor,
        }
    }
}

impl Bank {
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

    pub fn load_and_execute_transactions(&self, batch: &TransactionBatch) {
        let sanitized_txs = batch.sanitized_transactions();
        debug!("processing transactions: {}", sanitized_txs.len());
    }

    pub fn unlock_accounts(&self, batch: &mut TransactionBatch) {
        if batch.needs_unlock() {
            batch.set_needs_unlock(false);
            self.rc
                .accounts
                .unlock_accounts(batch.sanitized_transactions().iter(), batch.lock_results())
        }
    }
}

#[cfg(test)]
mod test {
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
        bank.load_and_execute_transactions(&batch);
    }
}
