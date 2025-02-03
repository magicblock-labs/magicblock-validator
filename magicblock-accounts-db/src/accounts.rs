use std::sync::{Arc, Mutex};

use log::debug;
use solana_accounts_db::accounts_index::ZeroLamport;
use solana_frozen_abi_macro::AbiExample;
use solana_sdk::{
    account::{AccountSharedData, ReadableAccount},
    address_lookup_table::{
        self, error::AddressLookupError, state::AddressLookupTable,
    },
    clock::Slot,
    message::{
        v0::{LoadedAddresses, MessageAddressTableLookup},
        AddressLoader, AddressLoaderError,
    },
    pubkey::Pubkey,
    slot_hashes::SlotHashes,
    transaction::{
        Result, SanitizedTransaction, TransactionAccountLocks, TransactionError,
    },
    transaction_context::TransactionAccount,
};
use solana_svm::{
    rollback_accounts::RollbackAccounts,
    transaction_processing_result::{
        ProcessedTransaction, TransactionProcessingResult,
        TransactionProcessingResultExtensions,
    },
};
use solana_svm_transaction::svm_message::SVMMessage;

use crate::{account_locks::AccountLocks, accounts_db::AccountsDb};

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

    pub fn set_slot(&self, slot: Slot) {
        self.accounts_db.set_slot(slot);
    }

    // -----------------
    // Load/Store Accounts
    // -----------------
    pub fn store_accounts_cached(
        &self,
        slot: Slot,
        accounts: Vec<(Pubkey, AccountSharedData)>,
    ) {
        self.accounts_db.store_cached(slot, accounts)
    }

    /// Store the accounts into the DB
    // allow(clippy) needed for various gating flags
    #[allow(clippy::too_many_arguments)]
    pub fn store_cached(
        &self,
        slot: Slot,
        txs: &[SanitizedTransaction],
        res: &[TransactionProcessingResult],
    ) {
        let accounts_to_store = Self::collect_accounts_to_store(txs, res);
        self.accounts_db.store_cached(slot, accounts_to_store);
    }

    #[allow(clippy::too_many_arguments)]
    fn collect_accounts_to_store<'a, T: SVMMessage>(
        txs: &'a [T],
        processing_results: &'a [TransactionProcessingResult],
    ) -> Vec<(Pubkey, AccountSharedData)> {
        let collect_capacity =
            max_number_of_accounts_to_collect(txs, processing_results);
        let mut accounts = Vec::with_capacity(collect_capacity);

        for (processing_result, transaction) in
            processing_results.iter().zip(txs)
        {
            let Some(processed_tx) = processing_result.processed_transaction()
            else {
                // Don't store any accounts if tx wasn't executed
                continue;
            };

            match processed_tx {
                ProcessedTransaction::Executed(executed_tx) => {
                    if executed_tx.execution_details.status.is_ok() {
                        collect_accounts_for_successful_tx(
                            &mut accounts,
                            transaction,
                            &executed_tx.loaded_transaction.accounts,
                        );
                    } else {
                        collect_accounts_for_failed_tx(
                            &mut accounts,
                            transaction,
                            &executed_tx.loaded_transaction.rollback_accounts,
                        );
                    }
                }
                ProcessedTransaction::FeesOnly(fees_only_tx) => {
                    collect_accounts_for_failed_tx(
                        &mut accounts,
                        transaction,
                        &fees_only_tx.rollback_accounts,
                    );
                }
            }
        }
        accounts
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
            |_pubkey, account| {
                Self::load_while_filtering(account, |account| {
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
            |_pubkey, account| {
                Self::load_while_filtering(account, |account| {
                    account.owner() == program_id && filter(account)
                })
            },
            config,
        )
    }

    fn load_while_filtering<F: Fn(&AccountSharedData) -> bool>(
        account: AccountSharedData,
        filter: F,
    ) -> bool {
        account.lamports() != 0 && filter(&account)
    }

    pub fn load_lookup_table_addresses(
        &self,
        current_slot: Slot,
        address_table_lookup: &MessageAddressTableLookup,
        slot_hashes: &SlotHashes,
    ) -> std::result::Result<LoadedAddresses, AddressLoaderError> {
        let table_account = self
            .accounts_db
            .load(&address_table_lookup.account_key)
            .ok_or(AddressLoaderError::LookupTableAccountNotFound)?;

        if table_account.owner() == &address_lookup_table::program::id() {
            let lookup_table = AddressLookupTable::deserialize(
                table_account.data(),
            )
            .map_err(|_ix_err| AddressLoaderError::InvalidAccountData)?;

            Ok(LoadedAddresses {
                writable: lookup_table
                    .lookup(
                        current_slot,
                        &address_table_lookup.writable_indexes,
                        slot_hashes,
                    )
                    .map_err(|_| {
                        AddressLoaderError::LookupTableAccountNotFound
                    })?,
                readonly: lookup_table
                    .lookup(
                        current_slot,
                        &address_table_lookup.readonly_indexes,
                        slot_hashes,
                    )
                    .map_err(|_| {
                        AddressLoaderError::LookupTableAccountNotFound
                    })?,
            })
        } else {
            Err(AddressLoaderError::InvalidAccountOwner)
        }
    }

    pub fn load_all(&self, sorted: bool) -> Vec<TransactionAccount> {
        self.accounts_db.scan_accounts(
            |_pubkey, account| account.lamports() != 0,
            &solana_accounts_db::accounts_index::ScanConfig::new(!sorted),
        )
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

fn collect_accounts_for_successful_tx<'a, T: SVMMessage>(
    collected_accounts: &mut Vec<(Pubkey, AccountSharedData)>,
    transaction: &'a T,
    transaction_accounts: &'a [TransactionAccount],
) {
    for (i, (address, account)) in
        (0..transaction.account_keys().len()).zip(transaction_accounts)
    {
        if !transaction.is_writable(i) {
            continue;
        }

        // Accounts that are invoked and also not passed as an instruction
        // account to a program don't need to be stored because it's assumed
        // to be impossible for a committable transaction to modify an
        // invoked account if said account isn't passed to some program.
        if transaction.is_invoked(i) && !transaction.is_instruction_account(i) {
            continue;
        }

        collected_accounts.push((*address, account.clone()));
    }
}

fn collect_accounts_for_failed_tx<'a, T: SVMMessage>(
    collected_accounts: &mut Vec<(Pubkey, AccountSharedData)>,
    transaction: &'a T,
    rollback_accounts: &'a RollbackAccounts,
) {
    let fee_payer_address = transaction.fee_payer();
    match rollback_accounts {
        RollbackAccounts::FeePayerOnly { fee_payer_account } => {
            collected_accounts
                .push((*fee_payer_address, fee_payer_account.clone()));
        }
        RollbackAccounts::SameNonceAndFeePayer { nonce } => {
            collected_accounts
                .push((*nonce.address(), nonce.account().clone()));
        }
        RollbackAccounts::SeparateNonceAndFeePayer {
            nonce,
            fee_payer_account,
        } => {
            collected_accounts
                .push((*fee_payer_address, fee_payer_account.clone()));

            collected_accounts
                .push((*nonce.address(), nonce.account().clone()));
        }
    }
}
fn max_number_of_accounts_to_collect(
    txs: &[impl SVMMessage],
    processing_results: &[TransactionProcessingResult],
) -> usize {
    processing_results
        .iter()
        .zip(txs)
        .filter_map(|(processing_result, tx)| {
            processing_result
                .processed_transaction()
                .map(|processed_tx| (processed_tx, tx))
        })
        .map(|(processed_tx, tx)| match processed_tx {
            ProcessedTransaction::Executed(executed_tx) => {
                match executed_tx.execution_details.status {
                    Ok(_) => tx.num_write_locks() as usize,
                    Err(_) => {
                        executed_tx.loaded_transaction.rollback_accounts.count()
                    }
                }
            }
            ProcessedTransaction::FeesOnly(fees_only_tx) => {
                fees_only_tx.rollback_accounts.count()
            }
        })
        .sum()
}
