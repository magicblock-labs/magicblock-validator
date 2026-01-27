use std::{fs, path::Path, sync::Arc, thread};

use error::{AccountsDbError, LogErr};
use index::{
    iterator::OffsetPubkeyIter, utils::AccountOffsetFinder, AccountsDbIndex,
};
use lmdb::{RwTransaction, Transaction};
use magicblock_config::config::AccountsDbConfig;
use parking_lot::RwLock;
use solana_account::{
    cow::AccountBorrowed, AccountSharedData, ReadableAccount,
};
use solana_pubkey::Pubkey;
use storage::AccountsStorage;
use tracing::{error, info, warn};

use crate::{snapshot::SnapshotManager, traits::AccountsBank};

pub type AccountsDbResult<T> = Result<T, AccountsDbError>;

/// A global lock used to suspend all write operations during critical
/// sections (like snapshots).
pub type GlobalWriteLock = Arc<RwLock<()>>;

pub const ACCOUNTSDB_DIR: &str = "accountsdb";

/// The main Accounts Database.
///
/// Coordinates:
/// 1. **Storage**: Append-only memory-mapped log (`AccountsStorage`).
/// 2. **Indexing**: LMDB-based key-value store (`AccountsDbIndex`).
/// 3. **Persistence**: Snapshot management (`SnapshotManager`).
#[cfg_attr(test, derive(Debug))]
pub struct AccountsDb {
    /// Underlying append-only storage for account data.
    pub storage: AccountsStorage,
    /// Fast index for account lookups (Pubkey -> Offset).
    index: AccountsDbIndex,
    /// Manages snapshots and state restoration.
    snapshot_manager: Arc<SnapshotManager>,
    /// Global lock ensures atomic snapshots by pausing writes.
    /// Note: Reads are generally wait-free/lock-free via mmap,
    /// unless they require index cursor stability.
    write_lock: GlobalWriteLock,
    /// Configured interval (in slots) for creating snapshots.
    snapshot_frequency: u64,
}

impl AccountsDb {
    /// Initializes the Accounts Database.
    ///
    /// This handles directory creation and potentially resets the state
    /// if `config.reset` is true.
    pub fn new(
        config: &AccountsDbConfig,
        root_dir: &Path,
        max_slot: u64,
    ) -> AccountsDbResult<Self> {
        let db_dir = root_dir.join(ACCOUNTSDB_DIR).join("main");

        if config.reset && db_dir.exists() {
            info!(db_path = %db_dir.display(), "Resetting AccountsDb");
            fs::remove_dir_all(&db_dir)
                .log_err(|| "Failed to reset accountsdb directory")?;
        }
        fs::create_dir_all(&db_dir)
            .log_err(|| "Failed to create accountsdb directory")?;

        let storage = AccountsStorage::new(config, &db_dir)
            .log_err(|| "Failed to initialize storage")?;

        let index = AccountsDbIndex::new(config.index_size, &db_dir)
            .log_err(|| "Failed to initialize index")?;

        let snapshot_manager =
            SnapshotManager::new(db_dir.clone(), config.max_snapshots as usize)
                .log_err(|| "Failed to initialize snapshot manager")?;

        if config.snapshot_frequency == 0 {
            return Err(AccountsDbError::Internal(
                "Snapshot frequency cannot be zero".to_string(),
            ));
        }

        let mut this = Self {
            storage,
            index,
            snapshot_manager,
            write_lock: GlobalWriteLock::default(),
            snapshot_frequency: config.snapshot_frequency,
        };

        // Recover state if the requested slot is older than our current state
        this.restore_state_if_needed(max_slot)?;

        Ok(this)
    }

    /// Opens an existing database (helper for tooling/tests).
    pub fn open(directory: &Path) -> AccountsDbResult<Self> {
        let config = AccountsDbConfig {
            snapshot_frequency: u64::MAX,
            ..Default::default()
        };
        Self::new(&config, directory, 0)
    }

    /// Inserts or updates a single account.
    pub fn insert_account(
        &self,
        pubkey: &Pubkey,
        account: &AccountSharedData,
    ) -> AccountsDbResult<()> {
        let mut txn = None;
        self.upsert(pubkey, account, &mut txn)?;
        if let Some(t) = txn {
            t.commit()?;
        }
        Ok(())
    }

    /// Inserts multiple accounts atomically.
    pub fn insert_batch<'a>(
        &self,
        accounts: impl Iterator<Item = &'a (Pubkey, AccountSharedData)> + Clone,
    ) -> AccountsDbResult<()> {
        let (mut txn, mut count) = (None, 0usize);

        for (pubkey, account) in accounts.clone() {
            match self.upsert(pubkey, account, &mut txn) {
                Ok(()) => count += 1,
                Err(e) => {
                    self.rollback(accounts, count);
                    return Err(e);
                }
            }
        }

        if let Some(Err(error)) = txn.map(|t| t.commit()) {
            error!(%error, "Batch insert commit failed");
            self.rollback(accounts, count);
            Err(error)?;
        }
        Ok(())
    }

    fn rollback<'a>(
        &self,
        accounts: impl Iterator<Item = &'a (Pubkey, AccountSharedData)>,
        count: usize,
    ) {
        for acc in accounts.take(count) {
            unsafe { acc.1.rollback() };
        }
    }

    fn upsert<'a>(
        &'a self,
        pubkey: &Pubkey,
        account: &AccountSharedData,
        txn: &mut Option<RwTransaction<'a>>,
    ) -> AccountsDbResult<()> {
        /// Get or create LMDB transaction for index writes.
        macro_rules! txn {
            () => {
                match txn.as_mut() {
                    Some(t) => t,
                    None => txn.get_or_insert(self.index.rwtxn()?),
                }
            };
        }
        match account {
            AccountSharedData::Borrowed(acc) => {
                if acc.owner_changed() {
                    self.index
                        .ensure_correct_owner(pubkey, account.owner(), txn!())
                        .log_err(|| "Failed to update owner index")?;
                }
                acc.commit();
            }
            AccountSharedData::Owned(acc) => {
                let data_len = account.data().len() as u32;
                let block_size = self.storage.block_size() as u32;
                let size_bytes = AccountSharedData::serialized_size_aligned(
                    data_len, block_size,
                ) as usize;
                let blocks_needed = self.storage.blocks_required(size_bytes);

                let result =
                    self.index.try_recycle_allocation(blocks_needed, txn!());
                let allocation = match result {
                    Ok(recycled) => {
                        self.storage.dec_recycled_count(recycled.blocks);
                        self.storage.recycle(recycled)
                    }
                    Err(AccountsDbError::NotFound) => {
                        self.storage.allocate(size_bytes)?
                    }
                    Err(e) => return Err(e),
                };

                let old_allocation = self
                    .index
                    .upsert_account(pubkey, account.owner(), allocation, txn!())
                    .log_err(|| "Index update failed")?;

                if let Some(old) = old_allocation {
                    self.storage.inc_recycled_count(old.blocks);
                }

                let ptr = allocation.ptr.as_ptr();
                let size = block_size * allocation.blocks;
                // SAFETY:
                // `allocation` provides a valid, exclusive pointer
                // range managed by `AccountsStorage`.
                unsafe { AccountSharedData::serialize_to_mmap(acc, ptr, size) };
            }
        }
        Ok(())
    }

    /// Checks if any of the `owners` own the `account`.
    pub fn account_matches_owners(
        &self,
        account: &Pubkey,
        owners: &[Pubkey],
    ) -> Option<usize> {
        let offset = self.index.get_offset(account).ok()?;
        let ptr = self.storage.resolve_ptr(offset);
        // SAFETY: `ptr` is guaranteed valid by `AccountsStorage` for the duration of the map.
        unsafe { AccountBorrowed::any_owner_matches(ptr.as_ptr(), owners) }
    }

    /// Returns a filterable iterator over accounts owned by `program`.
    pub fn get_program_accounts<F>(
        &self,
        program: &Pubkey,
        filter: F,
    ) -> AccountsDbResult<AccountsScanner<'_, F>>
    where
        F: Fn(&AccountSharedData) -> bool + 'static,
    {
        let iterator = self
            .index
            .get_program_accounts_iter(program)
            .log_err(|| "Failed to create program accounts iterator")?;

        Ok(AccountsScanner {
            iterator,
            storage: &self.storage,
            filter,
        })
    }

    /// An optimized accountsdb accessor, which can be used for multiple reads,
    /// without incurring the overhead of repeated creation of index transaction
    pub fn reader(&self) -> AccountsDbResult<AccountsReader<'_>> {
        let offset = self
            .index
            .offset_finder()
            .log_err(|| "Failed to create offset iterator")?;
        Ok(AccountsReader {
            offset,
            storage: &self.storage,
        })
    }

    /// Check whether pubkey is present in the AccountsDB
    pub fn contains_account(&self, pubkey: &Pubkey) -> bool {
        self.index.get_offset(pubkey).is_ok()
    }

    /// Return total number of accounts present in the database
    pub fn account_count(&self) -> usize {
        self.index.get_accounts_count()
    }

    /// Return the last slot written to AccountsDB
    #[inline(always)]
    pub fn slot(&self) -> u64 {
        self.storage.slot()
    }

    /// Updates the current slot. Triggers a background snapshot if the schedule matches.
    #[inline(always)]
    pub fn set_slot(self: &Arc<Self>, slot: u64) {
        self.storage.update_slot(slot);

        if slot > 0 && slot.is_multiple_of(self.snapshot_frequency) {
            self.trigger_background_snapshot(slot);
        }
    }

    /// Spawns a background thread to take a snapshot.
    fn trigger_background_snapshot(self: &Arc<Self>, slot: u64) {
        let this = self.clone();

        thread::spawn(move || {
            // Acquire write lock to ensure consistent state capture
            let write_guard = this.write_lock.write();
            this.flush();

            // Capture the active memory map region for the snapshot
            let used_storage = this.storage.active_segment();

            let _ = this.snapshot_manager.create_snapshot(
                slot,
                used_storage,
                write_guard,
            );
        });
    }

    /// Ensures the database state is at most `slot`.
    ///
    /// If the current state is newer than `slot`, this performs a **rollback**
    /// to the nearest valid snapshot.
    pub fn restore_state_if_needed(
        &mut self,
        target_slot: u64,
    ) -> AccountsDbResult<u64> {
        // Allow slot-1 because we might be in the middle of processing the current slot
        if target_slot >= self.slot().saturating_sub(1) {
            return Ok(self.slot());
        }

        warn!(
            current_slot = self.slot(),
            target_slot = target_slot,
            "Current slot ahead of target, rolling back"
        );

        // Block all writes during restoration
        let _guard = self.write_lock.write();

        let restored_slot = self
            .snapshot_manager
            .restore_from_snapshot(target_slot)
            .log_err(|| {
                format!("Snapshot restoration failed for target {target_slot}",)
            })?;

        // Reload components to reflect new FS state
        let path = self.snapshot_manager.database_path();
        self.storage.reload(path)?;
        self.index.reload(path)?;

        info!(restored_slot, "Successfully rolled back");
        Ok(restored_slot)
    }

    pub fn storage_size(&self) -> u64 {
        self.storage.size_bytes()
    }

    pub fn iter_all(
        &self,
    ) -> impl Iterator<Item = (Pubkey, AccountSharedData)> + '_ {
        let result = self.index.get_all_accounts().ok();
        let accounts = result.into_iter().flatten();
        accounts.map(|(offset, pk)| (pk, self.storage.read_account(offset)))
    }

    pub fn flush(&self) {
        self.storage.flush();
        self.index.flush();
    }

    pub fn write_lock(&self) -> GlobalWriteLock {
        self.write_lock.clone()
    }
}

impl AccountsBank for AccountsDb {
    #[inline(always)]
    fn get_account(&self, pubkey: &Pubkey) -> Option<AccountSharedData> {
        let offset = self.index.get_offset(pubkey).ok()?;
        Some(self.storage.read_account(offset))
    }

    fn remove_account(&self, pubkey: &Pubkey) {
        let Ok(mut txn) = self.index.rwtxn() else {
            error!("accountsdb: couldn't create rw index transaction");
            return;
        };
        let _ = self
            .index
            .remove(pubkey, &mut txn)
            .and_then(|_| txn.commit().map_err(Into::into))
            .log_err(|| format!("Failed to remove account {pubkey}"));
    }

    /// Removes accounts matching a predicate.
    fn remove_where(
        &self,
        predicate: impl Fn(&Pubkey, &AccountSharedData) -> bool,
    ) -> AccountsDbResult<usize> {
        let to_remove = self
            .iter_all()
            .filter_map(|(pk, acc)| predicate(&pk, &acc).then_some(pk))
            .collect::<Vec<_>>();

        let count = to_remove.len();
        let mut txn = self.index.rwtxn()?;
        for pubkey in to_remove {
            self.index.remove(&pubkey, &mut txn)?;
        }
        txn.commit()?;

        Ok(count)
    }
}

// SAFETY: AccountsDb uses internal locking (LMDB + RwLock) to ensure thread safety.
unsafe impl Sync for AccountsDb {}
unsafe impl Send for AccountsDb {}

pub struct AccountsScanner<'db, F> {
    storage: &'db AccountsStorage,
    filter: F,
    iterator: OffsetPubkeyIter<'db>,
}

impl<F> Iterator for AccountsScanner<'_, F>
where
    F: Fn(&AccountSharedData) -> bool,
{
    type Item = (Pubkey, AccountSharedData);
    fn next(&mut self) -> Option<Self::Item> {
        for (offset, pubkey) in self.iterator.by_ref() {
            let account = self.storage.read_account(offset);
            if (self.filter)(&account) {
                return Some((pubkey, account));
            }
        }
        None
    }
}

pub struct AccountsReader<'db> {
    offset: AccountOffsetFinder<'db>,
    storage: &'db AccountsStorage,
}

unsafe impl Send for AccountsReader<'_> {}
unsafe impl Sync for AccountsReader<'_> {}

impl AccountsReader<'_> {
    pub fn read<F, R>(&self, pubkey: &Pubkey, reader: F) -> Option<R>
    where
        F: Fn(AccountSharedData) -> R,
    {
        let offset = self.offset.find(pubkey)?;
        let account = self.storage.read_account(offset);
        Some(reader(account))
    }

    pub fn contains(&self, pubkey: &Pubkey) -> bool {
        self.offset.find(pubkey).is_some()
    }
}

#[cfg(test)]
impl AccountsDb {
    pub fn snapshot_exists(&self, slot: u64) -> bool {
        self.snapshot_manager.snapshot_exists(slot)
    }
}

pub mod error;
pub mod index;
mod snapshot;
mod storage;
#[cfg(test)]
mod tests;
pub mod traits;
