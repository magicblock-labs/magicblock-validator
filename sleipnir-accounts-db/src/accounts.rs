use crate::accounts_index::ZeroLamport;
use crate::storable_accounts::StorableAccounts;
use crate::{account_locks::AccountLocks, accounts_db::AccountsDb};
use log::debug;
use solana_frozen_abi_macro::AbiExample;
use solana_sdk::transaction::{
    Result, SanitizedTransaction, TransactionAccountLocks, TransactionError,
};
use solana_sdk::transaction_context::TransactionAccount;
use solana_sdk::{
    account::{AccountSharedData, ReadableAccount},
    clock::Slot,
    pubkey::Pubkey,
};
use std::sync::{Arc, Mutex};

#[derive(Debug, AbiExample)]
pub struct Accounts {
    /// Single global AccountsDb
    pub accounts_db: Arc<AccountsDb>,
    /// Set of read-only and writable accounts which are currently
    /// being processed by banking threads
    pub(crate) account_locks: Mutex<AccountLocks>,
}

impl Accounts {
    pub fn new(accounts_db: Arc<AccountsDb>) -> Self {
        Self {
            accounts_db,
            account_locks: Mutex::<AccountLocks>::default(),
        }
    }

    // -----------------
    // Load/Store Accounts
    // -----------------
    pub fn store_accounts_cached<
        'a,
        T: ReadableAccount + Sync + ZeroLamport + 'a,
    >(
        &self,
        accounts: impl StorableAccounts<'a, T>,
    ) {
        self.accounts_db.store_cached(accounts, None)
    }

    pub fn load_with_slot(
        &self,
        pubkey: &Pubkey,
    ) -> Option<(AccountSharedData, Slot)> {
        self.accounts_db.load_with_slot(pubkey)
    }

    pub fn load_by_program(
        &self,
        program_id: &Pubkey,
        config: &solana_accounts_db::accounts_index::ScanConfig,
    ) -> Vec<TransactionAccount> {
        self.accounts_db.scan_accounts(
            |pubkey, account| {
                Self::load_while_filtering(pubkey, account, |account| {
                    account.owner() == program_id
                })
            },
            config,
        )
    }

    pub fn load_by_program_with_filter<F>(
        &self,
        program_id: &Pubkey,
        filter: F,
        config: &solana_accounts_db::accounts_index::ScanConfig,
    ) -> Vec<TransactionAccount>
    where
        F: Fn(&AccountSharedData) -> bool + Send + Sync,
    {
        self.accounts_db.scan_accounts(
            |pubkey, account| {
                Self::load_while_filtering(pubkey, account, |account| {
                    account.owner() == program_id && filter(account)
                })
            },
            config,
        )
    }

    fn load_while_filtering<F: Fn(&AccountSharedData) -> bool>(
        pubkey: &Pubkey,
        account: AccountSharedData,
        filter: F,
    ) -> bool {
        !account.is_zero_lamport() && filter(&account)
    }

    // -----------------
    // Account Locks
    // -----------------
    /// This function will prevent multiple threads from modifying the same account state at the
    /// same time
    #[must_use]
    #[allow(clippy::needless_collect)]
    pub fn lock_accounts<'a>(
        &self,
        txs: impl Iterator<Item = &'a SanitizedTransaction>,
        tx_account_lock_limit: usize,
    ) -> Vec<Result<()>> {
        let tx_account_locks_results: Vec<Result<_>> = txs
            .map(|tx| tx.get_account_locks(tx_account_lock_limit))
            .collect();
        self.lock_accounts_inner(tx_account_locks_results)
    }

    #[must_use]
    #[allow(clippy::needless_collect)]
    pub fn lock_accounts_with_results<'a>(
        &self,
        txs: impl Iterator<Item = &'a SanitizedTransaction>,
        results: impl Iterator<Item = Result<()>>,
        tx_account_lock_limit: usize,
    ) -> Vec<Result<()>> {
        let tx_account_locks_results: Vec<Result<_>> = txs
            .zip(results)
            .map(|(tx, result)| match result {
                Ok(()) => tx.get_account_locks(tx_account_lock_limit),
                Err(err) => Err(err),
            })
            .collect();
        self.lock_accounts_inner(tx_account_locks_results)
    }

    #[must_use]
    fn lock_accounts_inner(
        &self,
        tx_account_locks_results: Vec<Result<TransactionAccountLocks>>,
    ) -> Vec<Result<()>> {
        let account_locks = &mut self.account_locks.lock().unwrap();
        tx_account_locks_results
            .into_iter()
            .map(|tx_account_locks_result| match tx_account_locks_result {
                Ok(tx_account_locks) => self.lock_account(
                    account_locks,
                    tx_account_locks.writable,
                    tx_account_locks.readonly,
                ),
                Err(err) => Err(err),
            })
            .collect()
    }

    fn lock_account(
        &self,
        account_locks: &mut AccountLocks,
        writable_keys: Vec<&Pubkey>,
        readonly_keys: Vec<&Pubkey>,
    ) -> Result<()> {
        for k in writable_keys.iter() {
            if account_locks.is_locked_write(k)
                || account_locks.is_locked_readonly(k)
            {
                debug!("Writable account in use: {:?}", k);
                return Err(TransactionError::AccountInUse);
            }
        }
        for k in readonly_keys.iter() {
            if account_locks.is_locked_write(k) {
                debug!("Read-only account in use: {:?}", k);
                return Err(TransactionError::AccountInUse);
            }
        }

        for k in writable_keys {
            account_locks.write_locks.insert(*k);
        }

        for k in readonly_keys {
            if !account_locks.lock_readonly(k) {
                account_locks.insert_new_readonly(k);
            }
        }

        Ok(())
    }

    /// Once accounts are unlocked, new transactions that modify that state can enter the pipeline
    #[allow(clippy::needless_collect)]
    pub fn unlock_accounts<'a>(
        &self,
        txs: impl Iterator<Item = &'a SanitizedTransaction>,
        results: &[Result<()>],
    ) {
        let keys: Vec<_> = txs
            .zip(results)
            .filter(|(_, res)| res.is_ok())
            .map(|(tx, _)| tx.get_account_locks_unchecked())
            .collect();
        let mut account_locks = self.account_locks.lock().unwrap();
        keys.into_iter().for_each(|keys| {
            self.unlock_account(
                &mut account_locks,
                keys.writable,
                keys.readonly,
            );
        });
    }

    fn unlock_account(
        &self,
        account_locks: &mut AccountLocks,
        writable_keys: Vec<&Pubkey>,
        readonly_keys: Vec<&Pubkey>,
    ) {
        for k in writable_keys {
            account_locks.unlock_write(k);
        }
        for k in readonly_keys {
            account_locks.unlock_readonly(k);
        }
    }
}
