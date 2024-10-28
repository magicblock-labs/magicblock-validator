use std::borrow::Borrow;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;

use crate::account_info::{AppendVecId, StorageLocation};
use crate::account_storage::meta::StorableAccountsWithHashesAndWriteVersions;
use crate::accounts_cache::SlotCache;
use crate::accounts_db::StoredMetaWriteVersion;
use crate::accounts_hash::AccountHash;
use crate::accounts_index::ZeroLamport;
use crate::append_vec::aligned_stored_size;
use crate::storable_accounts::StorableAccounts;
use crate::{
    account_info::AccountInfo,
    account_storage::{
        AccountStorage, AccountStorageEntry, AccountStorageStatus,
    },
};
use rand::{thread_rng, Rng};
use solana_measure::measure::Measure;
use solana_sdk::account::{AccountSharedData, ReadableAccount};
use solana_sdk::clock::Slot;
use solana_sdk::pubkey::Pubkey;

pub type AtomicAppendVecId = AtomicU32;

#[derive(Debug)]
pub struct AccountsPersister {
    storage: AccountStorage,
    paths: Vec<PathBuf>,
    /// distribute the accounts across storage lists
    next_id: AtomicAppendVecId,

    /// Write version used to notify accounts in order to distinguish between
    /// multiple updates to the same account in the same slot
    write_version: AtomicU64,
}

impl Default for AccountsPersister {
    fn default() -> Self {
        Self {
            storage: AccountStorage::default(),
            paths: Vec::new(),
            next_id: AtomicAppendVecId::new(0),
            write_version: AtomicU64::new(0),
        }
    }
}

impl AccountsPersister {
    pub(crate) fn new_with_paths(paths: Vec<PathBuf>) -> Self {
        Self {
            paths,
            ..Default::default()
        }
    }

    fn do_flush_slot_cache(&self, slot: Slot, slot_cache: &SlotCache) {
        let mut total_size = 0;

        let cached_accounts = slot_cache.iter().collect::<Vec<_>>();

        let (accounts, hashes): (
            Vec<(&Pubkey, &AccountSharedData)>,
            Vec<AccountHash>,
        ) = cached_accounts
            .iter()
            .map(|x| {
                let key = x.key();
                let account = &x.value().account;
                total_size += aligned_stored_size(account.data().len()) as u64;
                let hash = x.value().hash();

                ((key, account), hash)
            })
            .unzip();

        // Omitted purge_slot_cache_pubkey

        let is_dead_slot = accounts.is_empty();
        if !is_dead_slot {
            let flushed_store = self.create_and_insert_store(slot, total_size);
            let write_version_iterator: Box<dyn Iterator<Item = u64>> = {
                let mut current_version =
                    self.bulk_assign_write_version(accounts.len());
                Box::new(std::iter::from_fn(move || {
                    let ret = current_version;
                    current_version += 1;
                    Some(ret)
                }))
            };

            self.store_accounts_to(
                &(slot, &accounts[..]),
                hashes,
                write_version_iterator,
                &flushed_store,
            );
        }
    }

    fn store_accounts_to<
        'a: 'c,
        'b,
        'c,
        I: Iterator<Item = u64>,
        T: ReadableAccount + Sync + ZeroLamport + 'b,
    >(
        &self,
        accounts: &'c impl StorableAccounts<'b, T>,
        hashes: Vec<impl Borrow<AccountHash>>,
        mut write_version_iterator: I,
        storage: &Arc<AccountStorageEntry>,
    ) -> Vec<AccountInfo> {
        let slot = accounts.target_slot();
        if accounts.has_hash_and_write_version() {
            self.write_accounts_to_storage(
                slot,
                storage,
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
                .map(|_| write_version_iterator.next().unwrap())
                .collect::<Vec<_>>();
            self.write_accounts_to_storage(
                slot,
                storage,
                &StorableAccountsWithHashesAndWriteVersions::new_with_hashes_and_write_versions(
                    accounts,
                    hashes,
                    write_versions,
                ),
            )
        }
    }

    fn write_accounts_to_storage<'a, 'b, T, U, V>(
        &self,
        slot: Slot,
        storage: &AccountStorageEntry,
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
        let storage = self.create_and_insert_store(slot, size);

        let mut total_append_accounts_us = 0;
        while infos.len() < accounts_and_meta_to_store.len() {
            // Append accounts to storage entry
            let stored_accounts_info = {
                // TODO(thlorenz): metrics counter
                let mut append_accounts = Measure::start("append_accounts");
                let stored_accounts_info = storage
                    .accounts
                    .append_accounts(accounts_and_meta_to_store, infos.len());
                append_accounts.stop();
                total_append_accounts_us += append_accounts.as_us();

                stored_accounts_info
            };

            // Check if an account could not be stored due to storage being full
            let Some(stored_accounts_info) = stored_accounts_info else {
                storage.set_status(AccountStorageStatus::Full);

                // See if an account overflows the append vecs in the slot.
                // TODO(thlorenz): differing agave API to create another store
                todo!("Storage is full, but we don't handle this case yet");

                // continue;
            };

            let store_id = storage.append_vec_id();
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

    pub(crate) fn create_and_insert_store(
        &self,
        slot: Slot,
        size: u64,
    ) -> Arc<AccountStorageEntry> {
        self.create_and_insert_store_with_paths(slot, size, &self.paths)
    }

    fn create_and_insert_store_with_paths(
        &self,
        slot: Slot,
        size: u64,
        paths: &[PathBuf],
    ) -> Arc<AccountStorageEntry> {
        let store = self.create_store(slot, size, paths);
        let store_for_index = store.clone();

        self.insert_store(slot, store_for_index);
        store
    }

    fn insert_store(&self, slot: Slot, store: Arc<AccountStorageEntry>) {
        self.storage.insert(slot, store)
    }

    // -----------------
    // Create Store
    // -----------------
    fn create_store(
        &self,
        slot: Slot,
        size: u64,
        paths: &[PathBuf],
    ) -> Arc<AccountStorageEntry> {
        let path_index = thread_rng().gen_range(0..paths.len());
        Arc::new(self.new_storage_entry(
            slot,
            Path::new(&paths[path_index]),
            size,
        ))
    }

    fn new_storage_entry(
        &self,
        slot: Slot,
        path: &Path,
        size: u64,
    ) -> AccountStorageEntry {
        AccountStorageEntry::new(path, slot, self.next_id(), size)
    }

    fn next_id(&self) -> AppendVecId {
        let next_id = self.next_id.fetch_add(1, Ordering::AcqRel);
        assert!(next_id != AppendVecId::MAX, "We've run out of storage ids!");
        next_id
    }

    /// Increases [Self::write_version] by `count` and returns the previous value
    fn bulk_assign_write_version(
        &self,
        count: usize,
    ) -> StoredMetaWriteVersion {
        self.write_version
            .fetch_add(count as StoredMetaWriteVersion, Ordering::AcqRel)
    }

    // -----------------
    // Abondoned
    // -----------------
    /*
    pub(crate) fn store_accounts<'a, 'b, 'c, P, T>(
        &self,
        accounts: &'c impl StorableAccounts<'b, T>,
        hashes: Option<Vec<impl Borrow<AccountHash>>>,
        mut write_version_producer: P,
        slot: Slot,
    ) -> Vec<AccountInfo>
    where
        'a: 'b,
        'a: 'c,
        P: Iterator<Item = u64>,
        T: ReadableAccount + Sync + ZeroLamport + 'b,
    {
        // TODO(thlorenz): @@ figure out how to calculate a meaningful size
        let size = u64::MAX;
        if accounts.has_hash_and_write_version() {
            self.write_accounts_to_storage(
                &StorableAccountsWithHashesAndWriteVersions::<
                    '_,
                    '_,
                    _,
                    _,
                    &AccountHash,
                >::new(accounts),
                slot,
                size,
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
                    slot,
                    size,
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
                        slot,
                        size,
                    )
                }
            }
        }
    }
    */
}
