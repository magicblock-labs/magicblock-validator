// NOTE: copied and slightly modified from bank.rs
use std::{borrow::Cow, path::PathBuf, sync::Arc};

use solana_accounts_db::{
    accounts::Accounts,
    accounts_db::{
        AccountShrinkThreshold, AccountsDb, ACCOUNTS_DB_CONFIG_FOR_TESTING,
    },
    accounts_index::AccountSecondaryIndexes,
    accounts_update_notifier_interface::AccountsUpdateNotifier,
};
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
};

#[derive(Debug, Default)]
pub struct BankTestConfig {
    pub secondary_indexes: AccountSecondaryIndexes,
}

impl Bank {
    pub fn default_for_tests() -> Self {
        let accounts_db = AccountsDb::default_for_tests();
        let accounts = Accounts::new(Arc::new(accounts_db));
        Self::default_with_accounts(accounts)
    }

    pub fn new_for_tests(
        genesis_config: &GenesisConfig,
        accounts_update_notifier: Option<AccountsUpdateNotifier>,
        slot_status_notifier: Option<SlotStatusNotifierArc>,
    ) -> Self {
        Self::new_for_tests_with_config(
            genesis_config,
            BankTestConfig::default(),
            accounts_update_notifier,
            slot_status_notifier,
        )
    }

    pub fn new_for_tests_with_config(
        genesis_config: &GenesisConfig,
        test_config: BankTestConfig,
        accounts_update_notifier: Option<AccountsUpdateNotifier>,
        slot_status_notifier: Option<SlotStatusNotifierArc>,
    ) -> Self {
        Self::new_with_config_for_tests(
            genesis_config,
            test_config.secondary_indexes,
            AccountShrinkThreshold::default(),
            accounts_update_notifier,
            slot_status_notifier,
        )
    }

    pub(crate) fn new_with_config_for_tests(
        genesis_config: &GenesisConfig,
        account_indexes: AccountSecondaryIndexes,
        shrink_ratio: AccountShrinkThreshold,
        accounts_update_notifier: Option<AccountsUpdateNotifier>,
        slot_status_notifier: Option<SlotStatusNotifierArc>,
    ) -> Self {
        Self::new_with_paths_for_tests(
            genesis_config,
            Arc::new(RuntimeConfig::default()),
            Vec::new(),
            account_indexes,
            shrink_ratio,
            accounts_update_notifier,
            slot_status_notifier,
        )
    }

    pub fn new_with_paths_for_tests(
        genesis_config: &GenesisConfig,
        runtime_config: Arc<RuntimeConfig>,
        paths: Vec<PathBuf>,
        account_indexes: AccountSecondaryIndexes,
        shrink_ratio: AccountShrinkThreshold,
        accounts_update_notifier: Option<AccountsUpdateNotifier>,
        slot_status_notifier: Option<SlotStatusNotifierArc>,
    ) -> Self {
        let bank = Self::new_with_paths(
            genesis_config,
            runtime_config,
            paths,
            None,
            None,
            account_indexes,
            shrink_ratio,
            false,
            Some(ACCOUNTS_DB_CONFIG_FOR_TESTING),
            accounts_update_notifier,
            slot_status_notifier,
            Pubkey::new_unique(),
            Arc::default(),
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

    #[cfg(test)]
    pub fn flush_accounts_cache_slot_for_tests(&self) {
        self.rc
            .accounts
            .accounts_db
            .flush_accounts_cache_slot_for_tests(self.slot())
    }

    /// This is only valid to call from tests.
    /// block until initial accounts hash verification has completed
    pub fn wait_for_initial_accounts_hash_verification_completed_for_tests(
        &self,
    ) {
        self.rc
            .accounts
            .accounts_db
            .verify_accounts_hash_in_bg
            .wait_for_complete()
    }

    // pub fn update_accounts_hash_for_tests(&self) -> AccountsHash {
    //     self.update_accounts_hash(CalcAccountsHashDataSource::IndexForTests, false, false)
    // }
}
