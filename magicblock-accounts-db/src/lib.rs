use std::{path::PathBuf, sync::Arc};

use parking_lot::RwLock;
use solana_account::{
    cow::AccountBorrowed, AccountSharedData, ReadableAccount,
};
use solana_pubkey::Pubkey;

use config::AdbConfig;
use error::AdbError;
use index::AdbIndex;
use snapshot::SnapshotEngine;
use storage::AccountsStorage;
pub type AdbResult<T> = Result<T, AdbError>;
/// Stop the World Lock, used to halt all writes to adb while
/// some critical operation is in action, e.g. snapshotting
pub type StWLock = Arc<RwLock<()>>;

static mut ADB_SNAPSHOT_FREQUENCY: u64 = 0;

macro_rules! inspecterr {
    ($result: expr, $msg: expr) => {
        $result
            .inspect_err(|err| ::log::error!("adb - {} error: {err}", $msg))?
    };
    ($result: expr, $msg: expr, @option) => {
        $result
            .inspect_err(|err| ::log::error!("adb - {} error: {err}", $msg))
            .ok()
    };
    ($result: expr, $msg: expr, @silent) => {
        match $result {
            Ok(v) => v,
            Err(error) => {
                ::log::error!("adb - {} error: {error}", $msg);
                return;
            }
        }
    };
}

#[repr(C)] // perf: storage and cache will be stored in two contigious cache lines
pub struct AccountsDb {
    /// Main accounts storage, where actual account records are kept
    storage: AccountsStorage,
    /// Index manager, used for various lookup operations
    index: AdbIndex,
    /// Snapshots manager, boxed for cache efficiency, as this field is rarely used
    snap: Box<SnapshotEngine>,
    /// Stop the world lock, currently used for snapshotting only
    lock: StWLock,
}

impl AccountsDb {
    /// Open or create accounts database
    pub fn new(config: &AdbConfig, lock: StWLock) -> AdbResult<Self> {
        inspecterr!(
            std::fs::create_dir_all(&config.directory),
            "ensuring existence of accountsdb directory"
        );
        let storage =
            inspecterr!(AccountsStorage::new(config), "storage creation");
        let index = inspecterr!(AdbIndex::new(config), "index creation");
        let snap = inspecterr!(
            SnapshotEngine::new(config.directory.clone(), config.max_snapshots),
            "snapshot engine creation"
        );
        // no need to store global constants in type, this
        // is the only place it's set, so its use is safe
        unsafe { ADB_SNAPSHOT_FREQUENCY = config.snapshot_frequency };
        Ok(Self {
            storage,
            index,
            snap,
            lock,
        })
    }

    /// Opens existing database with given snapshot_frequency, used for tests and tools
    /// most likely you want to use [new](AccountsDb::new) method
    pub fn open(directory: PathBuf) -> AdbResult<Self> {
        let config = AdbConfig {
            directory,
            snapshot_frequency: u64::MAX,
            ..Default::default()
        };
        Self::new(&config, StWLock::default())
    }

    pub fn get_account(&self, pubkey: &Pubkey) -> AdbResult<AccountSharedData> {
        let offset = self.index.get_account_offset(pubkey)?;
        let memptr = self.storage.offset(offset);
        let account =
            unsafe { AccountSharedData::deserialize_from_mmap(memptr) };
        Ok(account.into())
    }

    pub fn insert_account(&self, pubkey: &Pubkey, account: &AccountSharedData) {
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
                let allocation = match self.index.allocation_exists(blocks) {
                    // if we could recycle some "hole" in database, use it
                    Ok(recycled) => {
                        // bookkeeping for deallocated(free hole) space
                        self.storage.decrement_deallocations(recycled.blocks);
                        self.storage.recycle(recycled)
                    }
                    // otherwise allocate from the end of memory map
                    Err(AdbError::NotFound) => self.storage.alloc(size),
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
                let dealloc = inspecterr!(
                    self.index.insert_account(
                        pubkey,
                        account.owner(),
                        allocation
                    ),
                    "account index insertion",
                    @silent
                );
                if let Some(dealloc) = dealloc {
                    // bookkeeping for deallocated (free hole) space
                    self.storage.increment_deallocations(dealloc.blocks);
                    // TODO(bmuddha): cleanup owner index as well
                    // after performing owner mismatch check
                }
            }
        }
    }

    pub fn account_matches_owners(
        &self,
        account: &Pubkey,
        owners: &[Pubkey],
    ) -> AdbResult<usize> {
        let offset = self.index.get_account_offset(account)?;
        let memptr = self.storage.offset(offset);
        // safety: memptr is obtained from storage directly,
        // which maintains the integrety of account records
        let position =
            unsafe { AccountBorrowed::any_owner_matches(memptr, owners) };
        position.ok_or(AdbError::NotFound)
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

        let iter = inspecterr!(
            self.index.get_program_accounts_iter(program),
            "program accounts retrieval"
        );
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

    pub fn contains_account(&self, pubkey: &Pubkey) -> bool {
        match self.index.get_account_offset(pubkey) {
            Ok(_) => true,
            Err(AdbError::NotFound) => false,
            Err(other) => {
                log::warn!("failed to check {pubkey} existence: {other}");
                false
            }
        }
    }

    #[inline(always)]
    pub fn slot(&self) -> u64 {
        self.storage.get_slot()
    }

    #[inline(always)]
    pub fn set_slot(&self, slot: u64) {
        self.storage.set_slot(slot);
        let remainder = unsafe { slot % ADB_SNAPSHOT_FREQUENCY };
        if remainder == 5 {
            // 5 slots before next snapshot point, start flushing asynchronously so
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

        if let Err(err) = self.snap.snapshot(slot) {
            // TODO: unclear what to do in such a situation, it's not like we can force snapshot at
            // this point (something must be terribly wrong, e.g. we run out of disk space), but at
            // the same time we can keep running the validator, should we just crash instead?
            log::error!(
                "error taking snapshot at {}-{slot}: {err}",
                self.snap.database_path().display()
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

        let rb_slot = inspecterr!(
            self.snap.try_switch_to_snapshot(slot),
            "switching to recent snapshot"
        );
        let path = self.snap.database_path();

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

    pub fn iter_all(
        &self,
    ) -> impl Iterator<Item = (Pubkey, AccountSharedData)> + '_ {
        let iter = inspecterr!(
            self.index.get_all_accounts(),
            "iterating all over all account keys",
            @option
        );
        iter.into_iter().flatten().map(|(offset, pk)| {
            let ptr = self.storage.offset(offset);
            let account =
                unsafe { AccountSharedData::deserialize_from_mmap(ptr) };
            (pk, account.into())
        })
    }

    pub fn flush(&self, sync: bool) {
        self.storage.flush(sync);
        // index is usually so small, that it takes a few ms at
        // most to flush it, so no need to schedule async flush
        if sync {
            self.index.flush();
        }
    }
}

unsafe impl Sync for AccountsDb {}
unsafe impl Send for AccountsDb {}

#[cfg(test)]
impl AccountsDb {
    pub fn snapshot_exists(&self, slot: u64) -> bool {
        self.snap.snapshot_exists(slot)
    }
}

pub mod config;
pub mod error;
mod index;
mod snapshot;
mod storage;
#[cfg(test)]
mod tests;
