use std::sync::{Arc, Mutex};

use log::debug;
pub use solana_accounts_db::accounts::TransactionLoadResult;
use solana_frozen_abi_macro::AbiExample;
use solana_sdk::{
    account::{AccountSharedData, ReadableAccount},
    account_utils::StateMut,
    address_lookup_table,
    address_lookup_table::{
        error::AddressLookupError, state::AddressLookupTable,
    },
    clock::Slot,
    message::v0::{LoadedAddresses, MessageAddressTableLookup},
    nonce::{
        state::{DurableNonce, Versions as NonceVersions},
        State as NonceState,
    },
    nonce_info::{NonceFull, NonceInfo},
    pubkey::Pubkey,
    slot_hashes::SlotHashes,
    transaction::{
        Result, SanitizedTransaction, TransactionAccountLocks, TransactionError,
    },
    transaction_context::TransactionAccount,
};

use crate::{
    account_locks::AccountLocks, accounts_db::AccountsDb,
    accounts_index::ZeroLamport, storable_accounts::StorableAccounts,
    transaction_results::TransactionExecutionResult,
};

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
    pub fn store_accounts_cached<
        'a,
        T: ReadableAccount + Sync + ZeroLamport + 'a,
    >(
        &self,
        accounts: impl StorableAccounts<'a, T>,
    ) {
        self.accounts_db.store_cached(accounts, None)
    }

    /// Store the accounts into the DB
    // allow(clippy) needed for various gating flags
    #[allow(clippy::too_many_arguments)]
    pub fn store_cached(
        &self,
        slot: Slot,
        txs: &[SanitizedTransaction],
        res: &[TransactionExecutionResult],
        loaded: &mut [TransactionLoadResult],
        durable_nonce: &DurableNonce,
        lamports_per_signature: u64,
    ) {
        let (accounts_to_store, transactions) = self.collect_accounts_to_store(
            txs,
            res,
            loaded,
            durable_nonce,
            lamports_per_signature,
        );
        self.accounts_db
            .store_cached((slot, &accounts_to_store[..]), Some(&transactions));
    }

    #[allow(clippy::too_many_arguments)]
    fn collect_accounts_to_store<'a>(
        &self,
        txs: &'a [SanitizedTransaction],
        execution_results: &'a [TransactionExecutionResult],
        load_results: &'a mut [TransactionLoadResult],
        durable_nonce: &DurableNonce,
        lamports_per_signature: u64,
    ) -> (
        Vec<(&'a Pubkey, &'a AccountSharedData)>,
        Vec<Option<&'a SanitizedTransaction>>,
    ) {
        let mut accounts = Vec::with_capacity(load_results.len());
        let mut transactions = Vec::with_capacity(load_results.len());
        for (i, ((tx_load_result, nonce), tx)) in
            load_results.iter_mut().zip(txs).enumerate()
        {
            if tx_load_result.is_err() {
                // Don't store any accounts if tx failed to load
                continue;
            }

            let execution_status = match &execution_results[i] {
                TransactionExecutionResult::Executed { details, .. } => {
                    &details.status
                }
                // Don't store any accounts if tx wasn't executed
                TransactionExecutionResult::NotExecuted(_) => continue,
            };

            let maybe_nonce = match (execution_status, &*nonce) {
                (Ok(_), _) => None, // Success, don't do any additional nonce processing
                (Err(_), Some(nonce)) => {
                    Some((nonce, true /* rollback */))
                }
                (Err(_), None) => {
                    // Fees for failed transactions which don't use durable nonces are
                    // deducted in Bank::filter_program_errors_and_collect_fee
                    continue;
                }
            };

            let message = tx.message();
            let loaded_transaction = tx_load_result.as_mut().unwrap();
            let mut fee_payer_index = None;
            for (i, (address, account)) in (0..message.account_keys().len())
                .zip(loaded_transaction.accounts.iter_mut())
                .filter(|(i, _)| message.is_non_loader_key(*i))
            {
                if fee_payer_index.is_none() {
                    fee_payer_index = Some(i);
                }
                let is_fee_payer = Some(i) == fee_payer_index;
                if message.is_writable(i) {
                    let is_nonce_account = prepare_if_nonce_account(
                        address,
                        account,
                        execution_status,
                        is_fee_payer,
                        maybe_nonce,
                        durable_nonce,
                        lamports_per_signature,
                    );

                    if execution_status.is_ok()
                        || is_nonce_account
                        || is_fee_payer
                    {
                        // Add to the accounts to store
                        accounts.push((&*address, &*account));
                        transactions.push(Some(tx));
                    }
                }
            }
        }
        (accounts, transactions)
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
        !account.is_zero_lamport() && filter(&account)
    }

    pub fn load_lookup_table_addresses(
        &self,
        current_slot: Slot,
        address_table_lookup: &MessageAddressTableLookup,
        slot_hashes: &SlotHashes,
    ) -> std::result::Result<LoadedAddresses, AddressLookupError> {
        let table_account = self
            .accounts_db
            .load(&address_table_lookup.account_key)
            .ok_or(AddressLookupError::LookupTableAccountNotFound)?;

        if table_account.owner() == &address_lookup_table::program::id() {
            let lookup_table = AddressLookupTable::deserialize(
                table_account.data(),
            )
            .map_err(|_ix_err| AddressLookupError::InvalidAccountData)?;

            Ok(LoadedAddresses {
                writable: lookup_table.lookup(
                    current_slot,
                    &address_table_lookup.writable_indexes,
                    slot_hashes,
                )?,
                readonly: lookup_table.lookup(
                    current_slot,
                    &address_table_lookup.readonly_indexes,
                    slot_hashes,
                )?,
            })
        } else {
            Err(AddressLookupError::InvalidAccountOwner)
        }
    }

    pub fn load_all(&self, sorted: bool) -> Vec<TransactionAccount> {
        self.accounts_db.scan_accounts(
            |_pubkey, account| !account.is_zero_lamport(),
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

fn prepare_if_nonce_account(
    address: &Pubkey,
    account: &mut AccountSharedData,
    execution_result: &Result<()>,
    is_fee_payer: bool,
    maybe_nonce: Option<(&NonceFull, bool)>,
    &durable_nonce: &DurableNonce,
    lamports_per_signature: u64,
) -> bool {
    if let Some((nonce, rollback)) = maybe_nonce {
        if address == nonce.address() {
            if rollback {
                // The transaction failed which would normally drop the account
                // processing changes, since this account is now being included
                // in the accounts written back to the db, roll it back to
                // pre-processing state.
                *account = nonce.account().clone();
            }

            // Advance the stored blockhash to prevent fee theft by someone
            // replaying nonce transactions that have failed with an
            // `InstructionError`.
            //
            // Since we know we are dealing with a valid nonce account,
            // unwrap is safe here
            let nonce_versions =
                StateMut::<NonceVersions>::state(nonce.account()).unwrap();
            if let NonceState::Initialized(ref data) = nonce_versions.state() {
                let nonce_state = NonceState::new_initialized(
                    &data.authority,
                    durable_nonce,
                    lamports_per_signature,
                );
                let nonce_versions = NonceVersions::new(nonce_state);
                account.set_state(&nonce_versions).unwrap();
            }
            true
        } else {
            if execution_result.is_err() && is_fee_payer {
                if let Some(fee_payer_account) = nonce.fee_payer_account() {
                    // Instruction error and fee-payer for this nonce tx is not
                    // the nonce account itself, rollback the fee payer to the
                    // fee-paid original state.
                    *account = fee_payer_account.clone();
                }
            }

            false
        }
    } else {
        false
    }
}
