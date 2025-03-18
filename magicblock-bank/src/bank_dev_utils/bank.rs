// NOTE: copied and slightly modified from bank.rs
use std::{borrow::Cow, sync::Arc};

use magicblock_accounts_db::{
    config::AccountsDbConfig, error::AccountsDbError, StWLock,
};
use solana_geyser_plugin_manager::slot_status_notifier::SlotStatusNotifierImpl;
use solana_sdk::{
    genesis_config::GenesisConfig,
    pubkey::Pubkey,
    transaction::{
        MessageHash, Result, SanitizedTransaction, Transaction,
        VersionedTransaction,
    },
};
use solana_svm::{
    runtime_config::RuntimeConfig,
    transaction_commit_result::TransactionCommitResult,
};
use solana_timings::ExecuteTimings;

use crate::{
    bank::Bank, geyser::AccountsUpdateNotifier,
    transaction_batch::TransactionBatch,
    transaction_logs::TransactionLogCollectorFilter,
    EPHEM_DEFAULT_MILLIS_PER_SLOT,
};

impl Bank {
    pub fn new_for_tests(
        genesis_config: &GenesisConfig,
        accounts_update_notifier: Option<AccountsUpdateNotifier>,
        slot_status_notifier: Option<SlotStatusNotifierImpl>,
    ) -> std::result::Result<Bank, AccountsDbError> {
        Self::new_with_config_for_tests(
            genesis_config,
            Arc::new(RuntimeConfig::default()),
            accounts_update_notifier,
            slot_status_notifier,
            EPHEM_DEFAULT_MILLIS_PER_SLOT,
        )
    }

    pub fn new_with_config_for_tests(
        genesis_config: &GenesisConfig,
        runtime_config: Arc<RuntimeConfig>,
        accounts_update_notifier: Option<AccountsUpdateNotifier>,
        slot_status_notifier: Option<SlotStatusNotifierImpl>,
        millis_per_slot: u64,
    ) -> std::result::Result<Bank, magicblock_accounts_db::error::AccountsDbError>
    {
        let accountsdb_config = AccountsDbConfig::temp_for_tests(500);
        let bank = Self::new(
            genesis_config,
            runtime_config,
            &accountsdb_config,
            None,
            None,
            false,
            accounts_update_notifier,
            slot_status_notifier,
            millis_per_slot,
            Pubkey::new_unique(),
            // TODO(bmuddha): when we switch to multithreaded mode,
            // switch to actual lock held by scheduler
            StWLock::default(),
        )?;
        bank.transaction_log_collector_config
            .write()
            .unwrap()
            .filter = TransactionLogCollectorFilter::All;
        Ok(bank)
    }

    /// Prepare a transaction batch from a list of legacy transactions. Used for tests only.
    pub fn prepare_batch_for_tests(
        &self,
        txs: Vec<Transaction>,
    ) -> TransactionBatch {
        let sanitized_txs = txs
            .into_iter()
            .map(SanitizedTransaction::from_transaction_for_tests)
            .collect::<Vec<_>>();
        let lock_results = vec![Ok(()); sanitized_txs.len()];
        TransactionBatch::new(lock_results, self, Cow::Owned(sanitized_txs))
    }

    /// Process multiple transaction in a single batch. This is used for benches and unit tests.
    ///
    /// # Panics
    ///
    /// Panics if any of the transactions do not pass sanitization checks.
    #[must_use]
    pub fn process_transactions<'a>(
        &self,
        txs: impl Iterator<Item = &'a Transaction>,
    ) -> Vec<TransactionCommitResult> {
        self.try_process_transactions(txs).unwrap()
    }

    /// Process entry transactions in a single batch. This is used for benches and unit tests.
    ///
    /// # Panics
    ///
    /// Panics if any of the transactions do not pass sanitization checks.
    #[must_use]
    pub fn process_entry_transactions(
        &self,
        txs: Vec<VersionedTransaction>,
    ) -> Vec<TransactionCommitResult> {
        self.try_process_entry_transactions(txs).unwrap()
    }

    /// Process a Transaction. This is used for unit tests and simply calls the vector
    /// Bank::process_transactions method.
    pub fn process_transaction(&self, tx: &Transaction) -> Result<()> {
        self.try_process_transactions(std::iter::once(tx))?[0].clone()?;
        tx.signatures
            .first()
            .map_or(Ok(()), |sig| self.get_signature_status(sig).unwrap())
    }

    /// Process multiple transaction in a single batch. This is used for benches and unit tests.
    /// Short circuits if any of the transactions do not pass sanitization checks.
    pub fn try_process_transactions<'a>(
        &self,
        txs: impl Iterator<Item = &'a Transaction>,
    ) -> Result<Vec<TransactionCommitResult>> {
        let txs = txs
            .map(|tx| VersionedTransaction::from(tx.clone()))
            .collect();
        self.try_process_entry_transactions(txs)
    }

    /// Process multiple transaction in a single batch. This is used for benches and unit tests.
    /// Short circuits if any of the transactions do not pass sanitization checks.
    pub fn try_process_entry_transactions(
        &self,
        txs: Vec<VersionedTransaction>,
    ) -> Result<Vec<TransactionCommitResult>> {
        let batch = self.prepare_entry_batch(txs)?;
        Ok(self.process_transaction_batch(&batch))
    }

    /// Prepare a transaction batch from a list of versioned transactions from
    /// an entry. Used for tests only.
    pub fn prepare_entry_batch(
        &self,
        txs: Vec<VersionedTransaction>,
    ) -> Result<TransactionBatch> {
        let sanitized_txs = txs
            .into_iter()
            .map(|tx| {
                SanitizedTransaction::try_create(
                    tx,
                    MessageHash::Compute,
                    None,
                    self,
                    &Default::default(),
                )
            })
            .collect::<Result<Vec<_>>>()?;
        let lock_results = vec![Ok(()); sanitized_txs.len()];
        Ok(TransactionBatch::new(
            lock_results,
            self,
            Cow::Owned(sanitized_txs),
        ))
    }

    #[must_use]
    pub(super) fn process_transaction_batch(
        &self,
        batch: &TransactionBatch,
    ) -> Vec<TransactionCommitResult> {
        self.load_execute_and_commit_transactions(
            batch,
            false,
            Default::default(),
            &mut ExecuteTimings::default(),
            None,
        )
        .0
    }
}
