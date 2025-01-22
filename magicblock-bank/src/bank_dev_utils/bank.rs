// NOTE: copied and slightly modified from bank.rs
use std::{borrow::Cow, path::PathBuf, sync::Arc};

use magicblock_accounts_db::{
    accounts::Accounts, accounts_db::AccountsDb,
    accounts_update_notifier_interface::AccountsUpdateNotifier,
};
use solana_program_runtime::timings::ExecuteTimings;
use solana_sdk::{
    genesis_config::GenesisConfig,
    pubkey::Pubkey,
    transaction::{
        MessageHash, Result, SanitizedTransaction, Transaction,
        VersionedTransaction,
    },
};
use solana_svm::runtime_config::RuntimeConfig;

use crate::{
    bank::Bank, slot_status_notifier_interface::SlotStatusNotifierArc,
    transaction_batch::TransactionBatch,
    transaction_logs::TransactionLogCollectorFilter,
    EPHEM_DEFAULT_MILLIS_PER_SLOT,
};

impl Bank {
    pub fn default_for_tests() -> Self {
        let accounts_db = AccountsDb::default_for_tests();
        let accounts = Accounts::new(Arc::new(accounts_db));
        let accounts_path = PathBuf::default();
        Self::default_with_accounts(
            accounts,
            accounts_path,
            EPHEM_DEFAULT_MILLIS_PER_SLOT,
        )
    }

    pub fn new_for_tests(
        genesis_config: &GenesisConfig,
        accounts_update_notifier: Option<AccountsUpdateNotifier>,
        slot_status_notifier: Option<SlotStatusNotifierArc>,
    ) -> Self {
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
        slot_status_notifier: Option<SlotStatusNotifierArc>,
        millis_per_slot: u64,
    ) -> Self {
        let account_paths = vec![PathBuf::default()];
        let bank = Self::new(
            genesis_config,
            runtime_config,
            None,
            None,
            false,
            account_paths,
            accounts_update_notifier,
            slot_status_notifier,
            millis_per_slot,
            Pubkey::new_unique(),
        );
        bank.transaction_log_collector_config
            .write()
            .unwrap()
            .filter = TransactionLogCollectorFilter::All;
        bank
    }

    /// Prepare a transaction batch from a list of legacy transactions. Used for tests only.
    pub fn prepare_batch_for_tests(
        &self,
        txs: Vec<Transaction>,
    ) -> TransactionBatch {
        let transaction_account_lock_limit =
            self.get_transaction_account_lock_limit();
        let sanitized_txs = txs
            .into_iter()
            .map(SanitizedTransaction::from_transaction_for_tests)
            .collect::<Vec<_>>();
        let lock_results = self.rc.accounts.lock_accounts(
            sanitized_txs.iter(),
            transaction_account_lock_limit,
        );
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
    ) -> Vec<Result<()>> {
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
    ) -> Vec<Result<()>> {
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
    ) -> Result<Vec<Result<()>>> {
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
    ) -> Result<Vec<Result<()>>> {
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
                )
            })
            .collect::<Result<Vec<_>>>()?;
        let tx_account_lock_limit = self.get_transaction_account_lock_limit();
        let lock_results = self
            .rc
            .accounts
            .lock_accounts(sanitized_txs.iter(), tx_account_lock_limit);
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
    ) -> Vec<Result<()>> {
        self.load_execute_and_commit_transactions(
            batch,
            false,
            Default::default(),
            &mut ExecuteTimings::default(),
            None,
        )
        .0
        .fee_collection_results
    }
}
