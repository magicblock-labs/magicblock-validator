use std::{
    borrow::Borrow,
    fs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU32, AtomicU64, Ordering},
        Arc,
    },
};

use crate::{
    account_info::{AccountInfo, AppendVecId, StorageLocation},
    account_storage::{
        meta::StorableAccountsWithHashesAndWriteVersions, AccountStorage,
        AccountStorageEntry, AccountStorageStatus,
    },
    accounts_cache::SlotCache,
    accounts_db::StoredMetaWriteVersion,
    accounts_hash::AccountHash,
    accounts_index::ZeroLamport,
    append_vec::{aligned_stored_size, STORE_META_OVERHEAD},
    errors::{AccountsDbError, AccountsDbResult},
    storable_accounts::StorableAccounts,
    DEFAULT_FILE_SIZE,
};
use log::*;
use rand::{thread_rng, Rng};
use solana_accounts_db::accounts_file::AccountsFile;
use solana_accounts_db::append_vec::AppendVec;
use solana_sdk::{
    account::{AccountSharedData, ReadableAccount},
    clock::Slot,
    pubkey::Pubkey,
};

pub type AtomicAppendVecId = AtomicU32;

/// The Accounts Persister is responsible for flushing accounts to disk
/// frequently. The flushed accounts remain in the cache, i.e. they
/// are not purged.
/// The disk usage is kept small by cleaning up older storage entries regularly.
#[derive(Debug)]
pub struct AccountsPersister {
    storage: AccountStorage,
    paths: Vec<PathBuf>,
    /// distribute the accounts across storage lists
    next_id: AtomicAppendVecId,

    /// Write version used to notify accounts in order to distinguish between
    /// multiple updates to the same account in the same slot
    write_version: AtomicU64,

    storage_cleanup_slot_freq: u64,
    last_storage_cleanup_slot: AtomicU64,

    file_size: u64,
}

/// How many slots pass each time before we flush accounts to disk
/// At 50ms per slot, this is every 25 seconds
/// This could be configurable in the future
pub const FLUSH_ACCOUNTS_SLOT_FREQ: u64 = 500;
impl Default for AccountsPersister {
    fn default() -> Self {
        Self {
            storage: AccountStorage::default(),
            paths: Vec::new(),
            next_id: AtomicAppendVecId::new(0),
            write_version: AtomicU64::new(0),
            storage_cleanup_slot_freq: 5 * FLUSH_ACCOUNTS_SLOT_FREQ,
            last_storage_cleanup_slot: AtomicU64::new(0),
            file_size: DEFAULT_FILE_SIZE,
        }
    }
}

impl AccountsPersister {
    pub fn new_with_paths(paths: Vec<PathBuf>) -> Self {
        Self {
            paths,
            ..Default::default()
        }
    }

    pub(crate) fn flush_slot_cache(
        &self,
        slot: Slot,
        slot_cache: &SlotCache,
    ) -> AccountsDbResult<u64> {
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

        // Clean up older storage entries regularly in order keep disk usage small
        if slot.saturating_sub(
            self.last_storage_cleanup_slot.load(Ordering::Relaxed),
        ) >= self.storage_cleanup_slot_freq
        {
            let keep_after =
                slot.saturating_sub(self.storage_cleanup_slot_freq);
            debug!(
                "cleanup_freq: {} slot: {} keep_after: {}",
                self.storage_cleanup_slot_freq, slot, keep_after
            );
            self.last_storage_cleanup_slot
                .store(slot, Ordering::Relaxed);
            Ok(self.delete_storage_entries_older_than(keep_after)?)
        } else {
            Ok(0)
        }
    }

    fn delete_storage_entries_older_than(
        &self,
        keep_after: Slot,
    ) -> Result<u64, std::io::Error> {
        fn warn_invalid_storage_path(entry: &fs::DirEntry) {
            warn!("Invalid storage file found at {:?}", entry.path());
        }

        if let Some(storage_path) = self.paths.first() {
            if !storage_path.exists() {
                warn!(
                    "Storage path does not exist to delete storage entries older than {}",
                    keep_after
                );
                return Ok(0);
            }

            let mut total_removed = 0;

            // Given the accounts path exists we cycle through all files stored in it
            // and clean out the ones that were saved before the given slot
            for entry in fs::read_dir(storage_path)? {
                let entry = entry?;
                if entry.metadata()?.is_dir() {
                    continue;
                } else if let Some(filename) = entry.file_name().to_str() {
                    // accounts are stored in a file with name `<slot>.<id>`
                    if let Some(slot) = filename.split('.').next() {
                        if let Ok(slot) = slot.parse::<Slot>() {
                            if slot <= keep_after {
                                fs::remove_file(entry.path())?;
                                total_removed += 1;
                            }
                        } else {
                            warn_invalid_storage_path(&entry);
                        }
                    } else {
                        warn_invalid_storage_path(&entry);
                    }
                }
            }
            Ok(total_removed)
        } else {
            warn!("No storage paths found to delete storage entries older than {}", keep_after);
            Ok(0)
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

        // This loop will continue until all accounts were appended to the `infos`
        // at which point their lengths will equal
        while infos.len() < accounts_and_meta_to_store.len() {
            // Append accounts to storage entry
            let stored_account_infos = storage
                .accounts
                .append_accounts(accounts_and_meta_to_store, infos.len());

            let Some(stored_account_infos) = stored_account_infos else {
                // An account could not be stored due to storage being full
                storage.set_status(AccountStorageStatus::Full);

                // See if an account overflows the append vecs in the slot.
                // This should not happen since we pass the total size of the accounts
                // we want to store when we create the storage entry.
                // See: flush_slot_cache
                // MAXIMUM_APPEND_VEC_FILE_SIZE is 16GB

                // Solana/Agave take the following steps:
                //
                // 1.  Verifies that an account is overflowing the available space of the
                //     appenc vecs in the slot
                // 2.  Calculates needed extra store size
                // 3a. attempts to reuse a recycled store (RecycleStore) and inserts that
                //     into storage
                // 3b. if that fails it creates a new store and inserts that into storage
                // 4.  after a new storage entry was inserted it continues at the top of the
                //     loop which will try this first step again
                //
                // In our implementation we left out 3.a and just always create a new store.
                //
                // NOTE: the part that I don't fully understand is that we don't change the
                //       `storage` variable at all, i.e. instead of reassigning it to the one
                //       returned from `create_and_insert_store`.
                //       I'm not sure if the storage entry is somehow able to expand into the
                //       new store we created, otherwise this implementation would be flawed.

                // Calculate needed size
                let account = accounts_and_meta_to_store.account(infos.len());
                let data_len = account
                    .map(|account| account.data().len())
                    .unwrap_or_default();
                let data_len = (data_len + STORE_META_OVERHEAD) as u64;
                // NOTE: I'm not sure how this would ever not be the case + this would lead
                // to looping endlessly if I'm not mistaken
                if !self.has_space_available(slot, data_len) {
                    info!(
                        "write_accounts_to_storage, no space: {}, {}, {}, {}, {}",
                        storage.accounts.capacity(),
                        storage.accounts.remaining_bytes(),
                        data_len,
                        infos.len(),
                        accounts_and_meta_to_store.len()
                    );
                    let special_store_size =
                        std::cmp::max(data_len * 2, self.file_size);
                    self.create_and_insert_store(slot, special_store_size);
                }
                continue;
            };

            // Once we ensured space for the accounts in the storage entry we push them
            // onto the infos
            let store_id = storage.append_vec_id();
            for (i, stored_account_info) in
                stored_account_infos.into_iter().enumerate()
            {
                storage.add_account(stored_account_info.size);

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

            storage.set_status(AccountStorageStatus::Available);
        }

        infos
    }

    // -----------------
    // Querying Storage
    // -----------------
    pub fn load_most_recent_store(
        &self,
    ) -> AccountsDbResult<AccountStorageEntry> {
        let path = self
            .paths
            .first()
            .ok_or(AccountsDbError::NoStoragePathProvided)?;

        // Read all files sorted slot/append_vec_id and return the last one
        let files = fs::read_dir(path)?;
        let mut files: Vec<_> = files
            .filter_map(|entry| {
                entry.ok().and_then(|entry| {
                    let path = entry.path();
                    let slot_and_id = path
                        .file_name()
                        .and_then(|file_name| file_name.to_str())
                        .and_then(|file_name| {
                            let parts =
                                file_name.split('.').collect::<Vec<_>>();
                            (parts.len() == 2).then(|| (parts[0], parts[1]))
                        })
                        .and_then(|(slot_str, append_vec_id_str)| {
                            let slot = slot_str.parse::<Slot>().ok();
                            let append_vec_id =
                                append_vec_id_str.parse::<AppendVecId>().ok();
                            if let (Some(slot), Some(append_vec_id)) =
                                (slot, append_vec_id)
                            {
                                Some((slot, append_vec_id))
                            } else {
                                None
                            }
                        });
                    slot_and_id.map(|(slot, id)| (path, slot, id))
                })
            })
            .collect();

        files.sort_by(
            |(_, slot_a, id_a): &(PathBuf, Slot, AppendVecId),
             (_, slot_b, id_b): &(PathBuf, Slot, AppendVecId)| {
                // Sorting in reverse order
                if slot_a == slot_b {
                    id_b.cmp(id_a)
                } else {
                    slot_b.cmp(slot_a)
                }
            },
        );
        let (file, slot, id) =
            files
                .first()
                .ok_or(AccountsDbError::NoAccountsFileFoundInside(
                    path.display().to_string(),
                ))?;

        // Create a AccountStorageEntry from the file
        let file_size = fs::metadata(file)?.len() as usize;
        let (append_vec, num_accounts) =
            AppendVec::new_from_file(file, file_size)?;
        let accounts = AccountsFile::AppendVec(append_vec);
        let storage = AccountStorageEntry::new_existing(
            *slot,
            *id,
            accounts,
            num_accounts,
        );
        Ok(storage)
    }

    // -----------------
    // Create Store
    // -----------------
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

    fn has_space_available(&self, slot: Slot, size: u64) -> bool {
        let store = self.storage.get_slot_storage_entry(slot).unwrap();
        if store.status() == AccountStorageStatus::Available
            && store.accounts.remaining_bytes() >= size
        {
            return true;
        }
        false
    }

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
    // Metrics
    // -----------------
    pub(crate) fn storage_size(
        &self,
    ) -> std::result::Result<u64, AccountsDbError> {
        // NOTE: at this point we assume that accounts are stored in only
        // one directory
        match self.paths.first() {
            Some(path) => Ok(fs_extra::dir::get_size(path)?),
            None => Ok(0),
        }
    }
}
