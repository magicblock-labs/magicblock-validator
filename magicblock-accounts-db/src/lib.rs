use std::{path::PathBuf, sync::Arc};

use parking_lot::RwLock;
use solana_account::{
    cow::AccountBorrowed, AccountSharedData, ReadableAccount,
};
use solana_pubkey::Pubkey;

use config::AccountsDbConfig;
use error::AccountsDbError;
use index::AccountsDbIndex;
use snapshot::SnapshotEngine;
use storage::AccountsStorage;
pub type AdbResult<T> = Result<T, AccountsDbError>;
/// Stop the World Lock, used to halt all writes to adb while
/// some critical operation is in action, e.g. snapshotting
pub type StWLock = Arc<RwLock<()>>;

#[repr(C)] // perf: storage and cache will be stored in two contigious cache lines
pub struct AccountsDb {
    /// Main accounts storage, where actual account records are kept
    storage: AccountsStorage,
    /// Index manager, used for various lookup operations
    index: AccountsDbIndex,
    /// Snapshots manager, boxed for cache efficiency, as this field is rarely used
    snapshot_engine: Box<SnapshotEngine>,
    /// Stop the world lock, currently used for snapshotting only
    lock: StWLock,
    /// Slot wise frequency at which snapshots should be taken
    snapshot_frequency: u64,
}

impl AccountsDb {
    /// Open or create accounts database
    pub fn new(config: &AccountsDbConfig, lock: StWLock) -> AdbResult<Self> {
        std::fs::create_dir_all(&config.directory).inspect_err(inspecterr!(
            "ensuring existence of accountsdb directory"
        ))?;
        let storage = AccountsStorage::new(config)
            .inspect_err(inspecterr!("storage creation"))?;
        let index = AccountsDbIndex::new(config)
            .inspect_err(inspecterr!("index creation"))?;
        let snapshot_engine =
            SnapshotEngine::new(config.directory.clone(), config.max_snapshots)
                .inspect_err(inspecterr!("snapshot engine creation"))?;
        let snapshot_frequency = config.snapshot_frequency;

        Ok(Self {
            storage,
            index,
            snapshot_engine,
            lock,
            snapshot_frequency,
        })
    }

    /// Opens existing database with given snapshot_frequency, used for tests and tools
    /// most likely you want to use [new](AccountsDb::new) method
    pub fn open(directory: PathBuf) -> AdbResult<Self> {
        let config = AccountsDbConfig {
            directory,
            snapshot_frequency: u64::MAX,
            ..Default::default()
        };
        Self::new(&config, StWLock::default())
    }

    /// Read account from with given pubkey from the database (if exists)
    pub fn get_account(&self, pubkey: &Pubkey) -> AdbResult<AccountSharedData> {
        let offset = self.index.get_account_offset(pubkey)?;
        let memptr = self.storage.offset(offset);
        let account =
            unsafe { AccountSharedData::deserialize_from_mmap(memptr) };
        Ok(account.into())
    }

    // TODO(bmuddha), under high load implement batch insertion to minimize index locking
    #[allow(unused)]
    pub fn insert_batch(&self, batch: &[(Pubkey, AccountSharedData)]) {}

    /// Insert account with given pubkey into the database
    /// Note: this method removes zero lamport account from database
    pub fn insert_account(&self, pubkey: &Pubkey, account: &AccountSharedData) {
        // don't store empty accounts
        if account.lamports() == 0 {
            let _ = self
                .index
                .remove_account(pubkey, account.owner())
                .inspect_err(inspecterr!("removing account {}", pubkey));
            return;
        }
        match account {
            AccountSharedData::Borrowed(acc) => {
                // this is the beauty of this AccountsDB implementation: when we have Borrowed
                // variant, we just increment atomic counter (nanosecond op) and that's it,
                // everything is already written, and new readers will now see the latest update
                acc.commit();
            }
            AccountSharedData::Owned(acc) => {
                let datalen = account.data().len();
                // we multiply by 2 for shadow buffer and add extra space for metadata
                let size = AccountSharedData::serialized_size_aligned(datalen)
                    * 2
                    + AccountSharedData::SERIALIZED_META_SIZE;

                let blocks = self.storage.get_block_count(size);
                // TODO(bmuddha) perf optimization: `allocation_exists` involves index lock + BTree
                // search and should ideally be used only when we have enough fragmentation to
                // increase the chances of finding perfect allocation in recyclable list. We should
                // utilize `AccountsStorage::fragmentation` to track fragmentation factor and start
                // recycling only when it exceeds some preconfigured threshold
                let allocation = match self.index.try_recycle_allocation(blocks)
                {
                    // if we could recycle some "hole" in database, use it
                    Ok(recycled) => {
                        // bookkeeping for deallocated(free hole) space
                        self.storage.decrement_deallocations(recycled.blocks);
                        self.storage.recycle(recycled)
                    }
                    // otherwise allocate from the end of memory map
                    Err(AccountsDbError::NotFound) => self.storage.alloc(size),
                    Err(other) => {
                        // This can only happen if we have catastrophic system mulfunction
                        log::error!("failed to insert account, index allocation check error: {other}");
                        return;
                    }
                };

                unsafe {
                    AccountSharedData::serialize_to_mmap(
                        acc,
                        allocation.storage,
                    )
                };
                // update accounts index
                let dealloc = self
                    .index
                    .insert_account(pubkey, account.owner(), allocation)
                    .inspect_err(inspecterr!("account index insertion"))
                    .ok()
                    .flatten();
                if let Some(dealloc) = dealloc {
                    // bookkeeping for deallocated (free hole) space
                    self.storage.increment_deallocations(dealloc.blocks);
                    // TODO(bmuddha): cleanup owner index as well
                    // after performing owner mismatch check
                }
            }
        }
    }

    /// Check whether given account is owned by any of the programs in the provided list
    pub fn account_matches_owners(
        &self,
        account: &Pubkey,
        owners: &[Pubkey],
    ) -> AdbResult<usize> {
        let offset = self.index.get_account_offset(account)?;
        let memptr = self.storage.offset(offset);
        // Safety: memptr is obtained from storage directly,
        // which maintains the integrety of account records
        let position =
            unsafe { AccountBorrowed::any_owner_matches(memptr, owners) };
        position.ok_or(AccountsDbError::NotFound)
    }

    /// Scan the database accounts of given program,
    /// satisfying provided filter
    pub fn get_program_accounts<F>(
        &self,
        program: &Pubkey,
        filter: F,
    ) -> AdbResult<Vec<(Pubkey, AccountSharedData)>>
    where
        F: Fn(&AccountSharedData) -> bool,
    {
        // TODO(bmuddha), perf optimization: F accepts AccountSharedData, we keep it
        // this way for now to preserve backward compatibility with solana rpc code,
        // but ideally filter should operate on AccountBorrowed or even the data field
        // alone (lamport filtering is not supported in solana).

        let iter = self
            .index
            .get_program_accounts_iter(program)
            .inspect_err(inspecterr!("program accounts retrieval"))?;
        let mut accounts = Vec::with_capacity(4);
        for (offset, pubkey) in iter {
            let memptr = self.storage.offset(offset);
            let account =
                unsafe { AccountSharedData::deserialize_from_mmap(memptr) };
            let account = AccountSharedData::Borrowed(account);

            if filter(&account) {
                accounts.push((pubkey, account));
            }
        }
        Ok(accounts)
    }

    /// Check whether account with given pubkey exists in the database
    pub fn contains_account(&self, pubkey: &Pubkey) -> bool {
        match self.index.get_account_offset(pubkey) {
            Ok(_) => true,
            Err(AccountsDbError::NotFound) => false,
            Err(other) => {
                log::warn!("failed to check {pubkey} existence: {other}");
                false
            }
        }
    }

    /// Get latest observed slot
    #[inline(always)]
    pub fn slot(&self) -> u64 {
        self.storage.get_slot()
    }

    /// Set latest observed slot
    #[inline(always)]
    pub fn set_slot(&self, slot: u64) {
        const PREEMPTIVE_FLUSHING_THRESHOLD: u64 = 5;
        self.storage.set_slot(slot);
        let remainder = slot % self.snapshot_frequency;
        if remainder == PREEMPTIVE_FLUSHING_THRESHOLD {
            // a few slots before next snapshot point, start flushing asynchronously so
            // that at the actual snapshot point there will be very little to flush
            self.flush(false);
            return;
        } else if remainder != 0 {
            return;
        }
        // acquire the lock, effectively stopping the world, nothing should be able
        // to modify underlying accounts database while this lock is active
        let _locked = self.lock.write();
        // flush everything before taking the snapshot, in order to ensure consistent state
        self.flush(true);

        if let Err(err) = self.snapshot_engine.snapshot(slot) {
            // TODO: unclear what to do in such a situation, it's not like we can force snapshot at
            // this point (something must be terribly wrong, e.g. we run out of disk space), but at
            // the same time we can keep running the validator, should we just crash instead?
            log::error!(
                "error taking snapshot at {}-{slot}: {err}",
                self.snapshot_engine.database_path().display()
            );
        }
    }

    /// Check whether AccountsDB has "freshness" not exceeding given slot
    /// Returns current slot if true, otherwise tries to rollback to the
    /// most recent snapshot, which is older than provided slot
    ///
    /// # Safety
    /// this is quite a dangerous method, which assumes that
    /// it runs only in the begining of startup, and only one reference
    /// to AccountsDb exists, making it exclusive one.
    ///
    /// Note: this will delete current database state upon rollback, use with care!
    pub unsafe fn ensure_at_most(&self, slot: u64) -> AdbResult<u64> {
        // TODO(bmuddha): redesign snapshot rollback in validator
        // so that this method can be called with proper &mut self

        // if this is a fresh start or we just match, then there's nothing to ensure
        if slot >= self.slot() {
            return Ok(self.slot());
        }
        // make sure that no one is reading the database
        let _locked = self.lock.write();

        let rb_slot = self
            .snapshot_engine
            .try_switch_to_snapshot(slot)
            .inspect_err(inspecterr!("switching to recent snapshot"))?;
        let path = self.snapshot_engine.database_path();

        // SAFETY: if assumption that the &self is exclusive is violated,
        // we have a UB with all sorts of terrible consequences
        #[allow(invalid_reference_casting)]
        let this = &mut *(self as *const Self as *mut Self);

        this.storage.reload(path)?;
        this.index.reload(path)?;
        Ok(rb_slot)
    }

    /// Get number total number of bytes in storage
    pub fn storage_size(&self) -> u64 {
        self.storage.size()
    }

    /// Returns an iterator over all accounts in the database,
    pub fn iter_all(
        &self,
    ) -> impl Iterator<Item = (Pubkey, AccountSharedData)> + '_ {
        let iter = self
            .index
            .get_all_accounts()
            .inspect_err(inspecterr!("iterating all over all account keys"))
            .ok();
        iter.into_iter().flatten().map(|(offset, pk)| {
            let ptr = self.storage.offset(offset);
            let account =
                unsafe { AccountSharedData::deserialize_from_mmap(ptr) };
            (pk, account.into())
        })
    }

    /// Flush primary storage and indexes to disk
    /// This operation can be done asynchronously (returning immediately)
    /// or in a blocking fashion
    pub fn flush(&self, sync: bool) {
        self.storage.flush(sync);
        // index is usually so small, that it takes a few ms at
        // most to flush it, so no need to schedule async flush
        if sync {
            self.index.flush();
        }
    }
}

// # Safety
// We only ever use AccountsDb within the Arc and all
// write access to it is synchronized via atomic operations
unsafe impl Sync for AccountsDb {}
unsafe impl Send for AccountsDb {}

#[cfg(test)]
impl AccountsDb {
    pub fn snapshot_exists(&self, slot: u64) -> bool {
        self.snapshot_engine.snapshot_exists(slot)
    }
}

pub mod config;
pub mod error;
mod index;
mod snapshot;
mod storage;
#[cfg(test)]
mod tests;
