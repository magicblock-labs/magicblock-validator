use std::{path::Path, sync::Arc};

use config::AccountsDbConfig;
use error::AccountsDbError;
use index::AccountsDbIndex;
use log::{error, warn};
use parking_lot::RwLock;
use snapshot::SnapshotEngine;
use solana_account::{
    cow::AccountBorrowed, AccountSharedData, ReadableAccount,
};
use solana_pubkey::Pubkey;
use storage::AccountsStorage;

use crate::snapshot::SnapSlot;

pub type AdbResult<T> = Result<T, AccountsDbError>;
/// Stop the World Lock, used to halt all writes to adb while
/// some critical operation is in action, e.g. snapshotting
pub type StWLock = Arc<RwLock<()>>;

const ACCOUNTSDB_SUB_DIR: &str = "accountsdb/main";

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
    pub fn new(
        config: &AccountsDbConfig,
        directory: &Path,
        lock: StWLock,
    ) -> AdbResult<Self> {
        let directory = directory.join(ACCOUNTSDB_SUB_DIR);

        std::fs::create_dir_all(&directory).inspect_err(log_err!(
            "ensuring existence of accountsdb directory"
        ))?;
        let storage = AccountsStorage::new(config, &directory)
            .inspect_err(log_err!("storage creation"))?;
        let index = AccountsDbIndex::new(config, &directory)
            .inspect_err(log_err!("index creation"))?;
        let snapshot_engine =
            SnapshotEngine::new(directory, config.max_snapshots as usize)
                .inspect_err(log_err!("snapshot engine creation"))?;
        let snapshot_frequency = config.snapshot_frequency;
        assert_ne!(snapshot_frequency, 0, "snapshot frequency cannot be zero");

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
    #[cfg(feature = "dev-tools")]
    pub fn open(directory: &Path) -> AdbResult<Self> {
        let config = AccountsDbConfig {
            snapshot_frequency: u64::MAX,
            ..Default::default()
        };
        Self::new(&config, directory, StWLock::default())
    }

    /// Read account from with given pubkey from the database (if exists)
    #[inline(always)]
    pub fn get_account(&self, pubkey: &Pubkey) -> AdbResult<AccountSharedData> {
        let offset = self.index.get_account_offset(pubkey)?;
        Ok(self.storage.read_account(offset))
    }

    /// Insert account with given pubkey into the database
    /// Note: this method removes zero lamport account from database
    pub fn insert_account(&self, pubkey: &Pubkey, account: &AccountSharedData) {
        // don't store empty accounts
        if account.lamports() == 0 {
            let _ = self.index.remove_account(pubkey).inspect_err(log_err!(
                "removing zero lamport account {}",
                pubkey
            ));
            return;
        }
        match account {
            AccountSharedData::Borrowed(acc) => {
                // For borrowed variants everything is already written and we just increment the
                // atomic counter. New readers will see the latest update.
                acc.commit();
                // and perform some index bookkeeping to ensure correct owner
                let _ = self
                    .index
                    .ensure_correct_owner(pubkey, account.owner())
                    .inspect_err(log_err!(
                        "failed to ensure correct account owner for {}",
                        pubkey
                    ));
            }
            AccountSharedData::Owned(acc) => {
                let datalen = account.data().len();
                // we multiply by 2 for shadow buffer and add extra space for metadata
                let size = AccountSharedData::serialized_size_aligned(datalen)
                    * 2
                    + AccountSharedData::SERIALIZED_META_SIZE;

                let blocks = self.storage.get_block_count(size);
                // TODO(bmuddha) perf optimization: use reallocs sparringly
                // https://github.com/magicblock-labs/magicblock-validator/issues/327
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
                    Err(err) => {
                        // This can only happen if we have catastrophic system mulfunction
                        error!("failed to insert account, index allocation check error: {err}");
                        return;
                    }
                };

                // SAFETY:
                // Allocation object is obtained by obtaining valid offset from storage, which
                // is unoccupied by other accounts, points to valid memory within mmap and is
                // properly aligned to 8 bytes, so the contract of serialize_to_mmap is satisfied
                unsafe {
                    AccountSharedData::serialize_to_mmap(
                        acc,
                        allocation.storage.as_ptr(),
                    )
                };
                // update accounts index
                let dealloc = self
                    .index
                    .insert_account(pubkey, account.owner(), allocation)
                    .inspect_err(log_err!("account index insertion"))
                    .ok()
                    .flatten();
                if let Some(dealloc) = dealloc {
                    // bookkeeping for deallocated (free hole) space
                    self.storage.increment_deallocations(dealloc.blocks);
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
        // SAFETY:
        // memptr is obtained from the storage directly, which maintains
        // the integrity of account records, by making sure, that they are
        // initialized and laid out properly along with the shadow buffer
        let position = unsafe {
            AccountBorrowed::any_owner_matches(memptr.as_ptr(), owners)
        };
        position.ok_or(AccountsDbError::NotFound)
    }

    /// Scans the database accounts of given program, satisfying the provided filter
    pub fn get_program_accounts<F>(
        &self,
        program: &Pubkey,
        filter: F,
    ) -> AdbResult<Vec<(Pubkey, AccountSharedData)>>
    where
        F: Fn(&AccountSharedData) -> bool,
    {
        // TODO(bmuddha): perf optimization in scanning logic
        // https://github.com/magicblock-labs/magicblock-validator/issues/328

        let iter = self
            .index
            .get_program_accounts_iter(program)
            .inspect_err(log_err!("program accounts retrieval"))?;
        let mut accounts = Vec::with_capacity(4);
        for (offset, pubkey) in iter {
            let account = self.storage.read_account(offset);

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
            Err(err) => {
                warn!("failed to check {pubkey} existence: {err}");
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

        let delta = self
            .snapshot_frequency
            .saturating_sub(PREEMPTIVE_FLUSHING_THRESHOLD);
        let preemptive_flush = delta != 0 && remainder == delta;

        if preemptive_flush {
            // a few slots before next snapshot point, start flushing asynchronously so
            // that at the actual snapshot point there will be very little to flush
            self.flush(false);
            return;
        }

        if remainder != 0 {
            return;
        }
        // acquire the lock, effectively stopping the world, nothing should be able
        // to modify underlying accounts database while this lock is active
        let _locked = self.lock.write();
        // flush everything before taking the snapshot, in order to ensure consistent state
        self.flush(true);

        let used_storage = self.storage.utilized_mmap();
        if let Err(err) = self.snapshot_engine.snapshot(slot, used_storage) {
            error!(
                "error taking snapshot at {}, slot {slot}: {err}",
                self.snapshot_engine.database_path().display()
            );
        }
    }

    /// Returns slot of latest snapshot or None
    /// Parses path to extract slot
    pub fn get_latest_snapshot_slot(&self) -> Option<u64> {
        self.snapshot_engine
            .with_snapshots(|snapshots| -> Option<u64> {
                let latest_path = snapshots.back()?;
                SnapSlot::try_from_path(latest_path)
                    .map(|snap_slot: SnapSlot| snap_slot.slot())
                    .or_else(|| {
                        error!(
                            "Failed to parse the path into SnapSlot: {}",
                            latest_path.display()
                        );
                        None
                    })
            })
    }

    /// Return slot of oldest maintained snapshot or None
    /// Parses path to extract slot
    pub fn get_oldest_snapshot_slot(&self) -> Option<u64> {
        self.snapshot_engine
            .with_snapshots(|snapshots| -> Option<u64> {
                let latest_path = snapshots.front()?;
                SnapSlot::try_from_path(latest_path)
                    .map(|snap_slot: SnapSlot| snap_slot.slot())
                    .or_else(|| {
                        error!(
                            "Failed to parse the path into SnapSlot: {}",
                            latest_path.display()
                        );
                        None
                    })
            })
    }

    /// Checks whether AccountsDB has "freshness", not exceeding given slot
    /// Returns current slot if true, otherwise tries to rollback to the
    /// most recent snapshot, which is older than the provided slot
    ///
    /// Note: this will delete the current database state upon rollback.
    /// But in most cases, the ledger slot and adb slot will match and
    /// no rollback will take place, in any case use with care!
    pub fn ensure_at_most(&mut self, slot: u64) -> AdbResult<u64> {
        // if this is a fresh start or we just match, then there's nothing to ensure
        if slot >= self.slot() - 1 {
            return Ok(self.slot());
        }
        // make sure that no one is reading the database
        let _locked = self.lock.write();

        let rb_slot = self
            .snapshot_engine
            .try_switch_to_snapshot(slot)
            .inspect_err(log_err!(
                "switching to snapshot befor slot {}",
                slot
            ))?;
        let path = self.snapshot_engine.database_path();

        self.storage.reload(path)?;
        self.index.reload(path)?;
        Ok(rb_slot)
    }

    /// Get the total number of bytes in storage
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
            .inspect_err(log_err!("iterating all over all account keys"))
            .ok();
        iter.into_iter()
            .flatten()
            .map(|(offset, pk)| (pk, self.storage.read_account(offset)))
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

// SAFETY:
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
