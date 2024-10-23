use std::borrow::Borrow;

use crate::account_info::StorageLocation;
use crate::account_storage::meta::StorableAccountsWithHashesAndWriteVersions;
use crate::accounts_hash::AccountHash;
use crate::accounts_index::ZeroLamport;
use crate::persist::hash_account::hash_account;
use crate::storable_accounts::StorableAccounts;
use crate::{
    account_info::AccountInfo,
    account_storage::{AccountStorageEntry, AccountStorageStatus},
};
use solana_measure::measure::Measure;
use solana_sdk::account::ReadableAccount;

pub struct AccountsPersister {
    storage: AccountStorageEntry,
}

impl AccountsPersister {
    pub(crate) fn new(storage: AccountStorageEntry) -> Self {
        Self { storage }
    }

    pub(crate) fn store_accounts<'a, 'b, 'c, P, T>(
        &self,
        accounts: &'c impl StorableAccounts<'b, T>,
        hashes: Option<Vec<impl Borrow<AccountHash>>>,
        mut write_version_producer: P,
    ) -> Vec<AccountInfo>
    where
        'a: 'c,
        P: Iterator<Item = u64>,
        T: ReadableAccount + Sync + ZeroLamport + 'b,
    {
        if accounts.has_hash_and_write_version() {
            self.write_accounts_to_storage(
                &StorableAccountsWithHashesAndWriteVersions::<
                    '_,
                    '_,
                    _,
                    _,
                    &AccountHash,
                >::new(accounts),
            )
        } else {
            let write_versions = (0..accounts.len())
                .map(|_| write_version_producer.next().unwrap())
                .collect::<Vec<_>>();
            match hashes {
                Some(hashes) => self.write_accounts_to_storage(
                    &StorableAccountsWithHashesAndWriteVersions::new_with_hashes_and_write_versions(
                        accounts,
                        hashes,
                        write_versions,
                    ),
                ),
                None => {
                    // hash any accounts where we were lazy in calculating the hash
                    let len = accounts.len();
                    let mut hashes = Vec::with_capacity(len);
                    for index in 0..accounts.len() {
                        let (pubkey, account) = (accounts.pubkey(index), accounts.account(index));
                        let hash = hash_account(
                            account,
                            pubkey,
                        );
                        hashes.push(hash);
                    }

                    self.write_accounts_to_storage(
                        &StorableAccountsWithHashesAndWriteVersions::new_with_hashes_and_write_versions(accounts, hashes, write_versions),
                    )
                }
            }
        }
    }

    fn write_accounts_to_storage<'a, 'b, T, U, V>(
        &self,
        accounts_and_meta_to_store: &StorableAccountsWithHashesAndWriteVersions<
            'a,
            'b,
            T,
            U,
            V,
        >,
    ) -> Vec<AccountInfo>
    where
        T: ReadableAccount + Sync,
        U: StorableAccounts<'a, T>,
        V: Borrow<AccountHash>,
    {
        let mut infos: Vec<AccountInfo> =
            Vec::with_capacity(accounts_and_meta_to_store.len());

        let mut total_append_accounts_us = 0;
        while infos.len() < accounts_and_meta_to_store.len() {
            // Append accounts to storage entry
            let stored_accounts_info = {
                // TODO(thlorenz): metrics counter
                let mut append_accounts = Measure::start("append_accounts");
                let stored_accounts_info = self
                    .storage
                    .accounts
                    .append_accounts(accounts_and_meta_to_store, infos.len());
                append_accounts.stop();
                total_append_accounts_us += append_accounts.as_us();

                stored_accounts_info
            };

            // Check if an account could not be stored due to storage being full
            let Some(stored_accounts_info) = stored_accounts_info else {
                self.storage.set_status(AccountStorageStatus::Full);

                // See if an account overflows the append vecs in the slot.
                // TODO(thlorenz): differing agave API to create another store
                todo!("Storage is full, but we don't handle this case yet");

                // continue;
            };

            let store_id = self.storage.append_vec_id();
            for (i, stored_account_info) in
                stored_accounts_info.into_iter().enumerate()
            {
                infos.push(AccountInfo::new(
                    StorageLocation::AppendVec(
                        store_id,
                        stored_account_info.offset,
                    ),
                    accounts_and_meta_to_store
                        .account(i)
                        .map(|account| account.lamports())
                        .unwrap_or_default(),
                ));
            }
        }

        infos
    }
}
