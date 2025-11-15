use std::{collections::HashSet, path::Path, sync::Arc};

use error::AccountsDbError;
use index::{
    iterator::OffsetPubkeyIter, utils::AccountOffsetFinder, AccountsDbIndex,
};
use log::{error, warn};
use magicblock_config::AccountsDbConfig;
use magicblock_core::traits::AccountsBank;
use parking_lot::RwLock;
use snapshot::SnapshotEngine;
use solana_account::{
    cow::AccountBorrowed, AccountSharedData, ReadableAccount,
};
use solana_pubkey::Pubkey;
use storage::AccountsStorage;

pub type AccountsDbResult<T> = Result<T, AccountsDbError>;
/// Stop the World Lock, used to halt all writes to the accountsdb
/// while some critical operation is in action, e.g. snapshotting
pub type StWLock = Arc<RwLock<()>>;

pub const ACCOUNTSDB_DIR: &str = "accountsdb";

#[cfg_attr(test, derive(Debug))]
pub struct AccountsDb {
    /// Main accounts storage, where actual account records are kept
    storage: AccountsStorage,
    /// Index manager, used for various lookup operations
    index: AccountsDbIndex,
    /// Snapshots manager
    snapshot_engine: Arc<SnapshotEngine>,
    /// Synchronization lock, employed for preventing other threads from
    /// writing to accountsdb, currently used for snapshotting only
    synchronizer: StWLock,
    /// Slot wise frequency at which snapshots should be taken
    snapshot_frequency: u64,
}

impl AccountsDb {
    /// Open or create accounts database
    pub fn new(
        config: &AccountsDbConfig,
        directory: &Path,
        max_slot: u64,
    ) -> AccountsDbResult<Self> {
        let directory = directory.join(format!("{ACCOUNTSDB_DIR}/main"));
        let lock = StWLock::default();

        std::fs::create_dir_all(&directory).inspect_err(log_err!(
            "ensuring existence of accountsdb directory"
        ))?;
        let storage = AccountsStorage::new(config, &directory)
            .inspect_err(log_err!("storage creation"))?;
        let index = AccountsDbIndex::new(config.index_map_size, &directory)
            .inspect_err(log_err!("index creation"))?;
        let snapshot_engine =
            SnapshotEngine::new(directory, config.max_snapshots as usize)
                .inspect_err(log_err!("snapshot engine creation"))?;
        let snapshot_frequency = config.snapshot_frequency;
        assert_ne!(snapshot_frequency, 0, "snapshot frequency cannot be zero");

        let mut this = Self {
            storage,
            index,
            snapshot_engine,
            synchronizer: lock,
            snapshot_frequency,
        };
        this.ensure_at_most(max_slot)?;
        Ok(this)
    }

    /// Opens existing database with given snapshot_frequency, used for tests and tools
    /// most likely you want to use [new](AccountsDb::new) method
    pub fn open(directory: &Path) -> AccountsDbResult<Self> {
        let config = AccountsDbConfig {
            snapshot_frequency: u64::MAX,
            ..Default::default()
        };
        Self::new(&config, directory, 0)
    }

    /// Insert account with given pubkey into the database
    /// Note: this method removes zero lamport account from database
    pub fn insert_account(&self, pubkey: &Pubkey, account: &AccountSharedData) {
        // NOTE: we don't check for non-zero lamports since we allow to store zero-lamport accounts
        // for the following two cases:
        // - when we clone a compressed account we reflect the exact lamports it has which maybe
        //   zero since compressed accounts don't need to be rent-exempt
        // - when we clone an account to signal that we fetched it from chain already but did not
        //   find it, i.e. in the case of an escrow account to avoid doing that over and over
        match account {
            AccountSharedData::Borrowed(acc) => {
                // For borrowed variants everything is already written and we just increment the
                // atomic counter. New readers will see the latest update.
                acc.commit();
                // check whether the account's owner has changed
                if !acc.owner_changed {
                    return;
                }
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
                let datalen = account.data().len() as u32;
                let block_size = self.storage.block_size() as u32;
                let size = AccountSharedData::serialized_size_aligned(
                    datalen, block_size,
                ) as usize;

                let blocks = self.storage.get_block_count(size);
                // TODO(bmuddha) perf optimization: use reallocs sparringly
                // https://github.com/magicblock-labs/magicblock-validator/issues/327
                let allocation = match self.index.try_recycle_allocation(blocks)
                {
                    // if we could recycle some "hole" in the database, use it
                    Ok(recycled) => {
                        // bookkeeping for the deallocated (free hole) space
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
                // Allocation object is constructed by obtaining a valid offset from storage, which
                // is unoccupied by other accounts, points to valid memory within mmap and is
                // properly aligned to 8 bytes, so the contract of serialize_to_mmap is satisfied
                unsafe {
                    AccountSharedData::serialize_to_mmap(
                        acc,
                        allocation.storage.as_ptr(),
                        block_size * allocation.blocks,
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
    ) -> Option<usize> {
        let offset = self.index.get_account_offset(account).ok()?;
        let memptr = self.storage.offset(offset);
        // SAFETY:
        // memptr is obtained from the storage directly, which maintains
        // the integrity of account records, by making sure, that they are
        // initialized and laid out properly along with the shadow buffer
        unsafe { AccountBorrowed::any_owner_matches(memptr.as_ptr(), owners) }
    }

    /// Scans the database accounts of given program, satisfying the provided filter
    pub fn get_program_accounts<F>(
        &self,
        program: &Pubkey,
        filter: F,
    ) -> AccountsDbResult<AccountsScanner<F>>
    where
        F: Fn(&AccountSharedData) -> bool + 'static,
    {
        // TODO(bmuddha): perf optimization in scanning logic
        // https://github.com/magicblock-labs/magicblock-validator/issues/328

        let iterator = self
            .index
            .get_program_accounts_iter(program)
            .inspect_err(log_err!("program accounts retrieval"))?;
        Ok(AccountsScanner {
            iterator,
            storage: &self.storage,
            filter,
        })
    }

    pub fn reader(&self) -> AccountsDbResult<AccountsReader<'_>> {
        let offset = self.index.offset_finder()?;
        Ok(AccountsReader {
            offset,
            storage: &self.storage,
        })
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

    /// Get the number of accounts in the database
    pub fn get_accounts_count(&self) -> usize {
        self.index.get_accounts_count()
    }

    /// Get latest observed slot
    #[inline(always)]
    pub fn slot(&self) -> u64 {
        self.storage.get_slot()
    }

    /// Temporary hack for overriding accountsdb slot without snapshot checks
    // TODO(bmuddha): remove with the ledger rewrite
    pub fn override_slot(&self, slot: u64) {
        self.storage.set_slot(slot);
    }

    /// Set latest observed slot
    #[inline(always)]
    pub fn set_slot(self: &Arc<Self>, slot: u64) {
        self.storage.set_slot(slot);

        if 0 != slot % self.snapshot_frequency {
            return;
        }
        let this = self.clone();
        // Since `set_slot` is usually invoked in async context, we don't want to
        // ever block it. Here we move the whole lock acquisition and snapshotting
        // to a separate thread, considering that snapshot taking is extremely rare
        // operation, the overhead should be negligible
        std::thread::spawn(move || {
            // acquire the lock, effectively stopping the world, nothing should be able
            // to modify underlying accounts database while this lock is active
            let locked = this.synchronizer.write();
            // flush everything before taking the snapshot, in order to ensure consistent state
            this.flush();

            let used_storage = this.storage.utilized_mmap();
            if let Err(err) =
                this.snapshot_engine.snapshot(slot, used_storage, locked)
            {
                warn!(
                    "failed to take snapshot at {}, slot {slot}: {err}",
                    this.snapshot_engine.database_path().display()
                );
            }
        });
    }

    /// Checks whether AccountsDB has "freshness", not exceeding given slot
    /// Returns current slot if true, otherwise tries to rollback to the
    /// most recent snapshot, which is older than the provided slot
    ///
    /// Note: this will delete the current database state upon rollback.
    /// But in most cases, the ledger slot and adb slot will match and
    /// no rollback will take place, in any case use with care!
    pub fn ensure_at_most(&mut self, slot: u64) -> AccountsDbResult<u64> {
        // if this is a fresh start or we just match, then there's nothing to ensure
        if slot >= self.slot().saturating_sub(1) {
            return Ok(self.slot());
        }
        // make sure that no one is reading the database
        let _locked = self.synchronizer.write();

        let rb_slot = self
            .snapshot_engine
            .try_switch_to_snapshot(slot)
            .inspect_err(log_err!(
                "switching to snapshot before slot {}",
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
    pub fn flush(&self) {
        self.storage.flush();
        self.index.flush();
    }

    /// Get a clone of synchronization lock, to suspend all the writes,
    /// while some critical operation, like snapshotting is in progress
    pub fn synchronizer(&self) -> StWLock {
        self.synchronizer.clone()
    }
}

impl AccountsBank for AccountsDb {
    /// Read account from with given pubkey from the database (if exists)
    #[inline(always)]
    fn get_account(&self, pubkey: &Pubkey) -> Option<AccountSharedData> {
        let offset = self.index.get_account_offset(pubkey).ok()?;
        Some(self.storage.read_account(offset))
    }

    fn remove_account(&self, pubkey: &Pubkey) {
        let _ = self
            .index
            .remove_account(pubkey)
            .inspect_err(log_err!("removing an account {}", pubkey));
    }

    /// Remove all accounts matching the provided predicate
    /// NOTE: accounts are not locked while this operation is in progress,
    /// thus this should only be performed before the validator starts processing
    /// transactions
    fn remove_where(
        &self,
        predicate: impl Fn(&Pubkey, &AccountSharedData) -> bool,
    ) -> usize {
        let to_remove = self
            .iter_all()
            .filter(|(pk, acc)| predicate(pk, acc))
            .map(|(pk, _)| pk)
            .collect::<HashSet<_>>();
        let removed = to_remove.len();
        for pk in to_remove {
            self.remove_account(&pk);
        }
        removed
    }
}

// SAFETY:
// We only ever use AccountsDb within the Arc and all
// write access to it is synchronized via atomic operations
unsafe impl Sync for AccountsDb {}
unsafe impl Send for AccountsDb {}

/// Iterator to scan program accounts applying filtering logic on them
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
        loop {
            let (offset, pubkey) = self.iterator.next()?;
            let account = self.storage.read_account(offset);
            if (self.filter)(&account) {
                break Some((pubkey, account));
            }
        }
    }
}

/// Versatile and reusable account reader, can be used to perform multiple account queries
/// from the database more efficiently, avoiding the cost of index/cursor setups
pub struct AccountsReader<'db> {
    offset: AccountOffsetFinder<'db>,
    storage: &'db AccountsStorage,
}

// SAFETY:
// AccountsReader is only ever used to get readable access to the
// underlying database, and never outlives the the backing storage
unsafe impl Send for AccountsReader<'_> {}
unsafe impl Sync for AccountsReader<'_> {}

impl AccountsReader<'_> {
    /// Find the account specified by the pubkey and pass it to the reader function
    pub fn read<F, R>(&self, pubkey: &Pubkey, reader: F) -> Option<R>
    where
        F: Fn(AccountSharedData) -> R,
    {
        let offset = self.offset.find(pubkey)?;
        let account = self.storage.read_account(offset);
        Some(reader(account))
    }

    /// Check whether given account exists in the AccountsDB
    pub fn contains(&self, pubkey: &Pubkey) -> bool {
        self.offset.find(pubkey).is_some()
    }
}

#[cfg(test)]
impl AccountsDb {
    pub fn snapshot_exists(&self, slot: u64) -> bool {
        self.snapshot_engine.snapshot_exists(slot)
    }
}

pub mod error;
mod index;
mod snapshot;
mod storage;
#[cfg(test)]
mod tests;
