use std::path::Path;

use iterator::OffsetPubkeyIter;
use lmdb::{Cursor, DatabaseFlags, Environment, RwTransaction, Transaction};
use solana_pubkey::Pubkey;
use table::Table;
use tracing::warn;
use utils::*;

use crate::{
    error::{AccountsDbError, LogErr},
    storage::{Allocation, ExistingAllocation},
    AccountsDbResult,
};

pub type Offset = u32;
pub type Blocks = u32;

const ACCOUNTS_INDEX: &str = "accounts-idx";
const PROGRAMS_INDEX: &str = "programs-idx";
const DEALLOCATIONS_INDEX: &str = "deallocations-idx";
const OWNERS_INDEX: &str = "owners-idx";

/// LMDB Index manager.
///
/// Handles secondary indices for mapping Pubkeys to storage offsets,
/// tracking program ownership, and managing deallocated space.
#[cfg_attr(test, derive(Debug))]
pub(crate) struct AccountsDbIndex {
    /// Maps Account Pubkey -> (Storage Offset, Block Count).
    accounts: Table,
    /// Maps Owner Pubkey -> (Storage Offset, Account Pubkey).
    /// Used for `get_program_accounts`.
    programs: Table,
    /// Maps Allocation Size (Blocks) -> (Storage Offset, Block Count).
    /// Used for finding recyclable "holes" in storage.
    deallocations: Table,
    /// Maps Account Pubkey -> Owner Pubkey.
    /// Used for cleaning up the `programs` index when an account changes owner.
    owners: Table,
    /// The LMDB Environment.
    env: Environment,
}

/// Helper macro to pack/unpack types into/from byte buffers.
/// Uses unaligned writes/reads for performance and compactness.
macro_rules! bytes {
    (#pack, $val1: expr, $type1: ty, $val2: expr, $type2: ty) => {{
        const S1: usize = std::mem::size_of::<$type1>();
        const S2: usize = std::mem::size_of::<$type2>();
        let mut buffer = [0_u8; S1 + S2];
        let ptr = buffer.as_mut_ptr();
        // SAFETY: Buffer is exactly S1 + S2 bytes.
        unsafe {
            (ptr as *mut $type1).write_unaligned($val1);
            (ptr.add(S1) as *mut $type2).write_unaligned($val2);
        }
        buffer
    }};
    (#unpack, $packed: expr, $type1: ty, $type2: ty) => {{
        let ptr = $packed.as_ptr();
        const S1: usize = std::mem::size_of::<$type1>();
        // SAFETY: Macro caller ensures $packed is valid length.
        unsafe {
            let v1 = (ptr as *const $type1).read_unaligned();
            let v2 = (ptr.add(S1) as *const $type2).read_unaligned();
            (v1, v2)
        }
    }};
}

impl AccountsDbIndex {
    pub(crate) fn new(size: usize, directory: &Path) -> AccountsDbResult<Self> {
        let env = utils::create_lmdb_env(directory, size).log_err(|| {
            format!("main index env creation at {}", directory.display())
        })?;

        let accounts =
            Table::new(&env, ACCOUNTS_INDEX, DatabaseFlags::empty())?;

        let programs = Table::new(
            &env,
            PROGRAMS_INDEX,
            DatabaseFlags::DUP_SORT | DatabaseFlags::DUP_FIXED,
        )?;

        // DEALLOCATIONS: Allow duplicates (multiple holes of same size),
        // integer keys (block size) for range searches.
        let deallocations = Table::new(
            &env,
            DEALLOCATIONS_INDEX,
            DatabaseFlags::DUP_SORT
                | DatabaseFlags::DUP_FIXED
                | DatabaseFlags::REVERSE_KEY
                | DatabaseFlags::INTEGER_KEY,
        )?;

        let owners = Table::new(&env, OWNERS_INDEX, DatabaseFlags::empty())?;

        Ok(Self {
            accounts,
            programs,
            deallocations,
            owners,
            env,
        })
    }

    /// Retrieves the storage offset for a given account.
    #[inline(always)]
    pub(crate) fn get_offset(
        &self,
        pubkey: &Pubkey,
    ) -> AccountsDbResult<Offset> {
        let txn = self.env.begin_ro_txn()?;
        let Some(value_bytes) = self.accounts.get(&txn, pubkey)? else {
            return Err(AccountsDbError::NotFound);
        };

        // We only need the first 4 bytes (Offset), ignoring the Blocks count.
        // SAFETY: Accounts index values are always created by `bytes!(#pack, ...)`
        // which guarantees [Offset(4), Blocks(4)].
        let offset =
            unsafe { (value_bytes.as_ptr() as *const Offset).read_unaligned() };
        Ok(offset)
    }

    /// Inserts or updates an account's allocation in the indices.
    ///
    /// If the account already exists, it handles the "move":
    /// 1. Marks the old space as deallocated (recyclable).
    /// 2. Updates the main index.
    /// 3. Updates secondary indices (programs, owners).
    pub(crate) fn upsert_account(
        &self,
        pubkey: &Pubkey,
        owner: &Pubkey,
        allocation: Allocation,
        txn: &mut RwTransaction,
    ) -> AccountsDbResult<Option<ExistingAllocation>> {
        let Allocation { offset, blocks, .. } = allocation;
        let mut old_allocation = None;

        let index_value = bytes!(#pack, offset, Offset, blocks, Blocks);
        let offset_and_pubkey = bytes!(#pack, offset, Offset, *pubkey, Pubkey);

        // 1. Optimistic insert (returns true if inserted, false if existed)
        let inserted = self.accounts.insert(txn, pubkey, index_value)?;

        // 2. Handle update if it already existed
        if !inserted {
            let previous =
                self.reallocate_account(pubkey, txn, &index_value)?;
            old_allocation = Some(previous);
        }

        // 3. Update secondary indices
        self.programs.upsert(txn, owner, offset_and_pubkey)?;
        self.owners.upsert(txn, pubkey, owner)?;

        Ok(old_allocation)
    }

    /// Handles the logistics of moving an existing account to a new location.
    /// Returns the old allocation so it can be added to the free list.
    fn reallocate_account(
        &self,
        pubkey: &Pubkey,
        txn: &mut RwTransaction,
        new_index_value: &[u8],
    ) -> AccountsDbResult<ExistingAllocation> {
        // Retrieve old location
        let old_alloc = self.get_allocation(txn, pubkey)?;

        // Mark old space as free (Deallocations Index)
        // Key: Block Count (for size-based lookup), Value: {Offset, Block Count}
        let key = old_alloc.blocks.to_le_bytes();
        let value =
            bytes!(#pack, old_alloc.offset, Offset, old_alloc.blocks, Blocks);
        self.deallocations.upsert(txn, key, value)?;

        // Update Main Index with new location
        self.accounts.upsert(txn, pubkey, new_index_value)?;

        // Clean up Programs Index (dangling pointer to old offset)
        self.remove_program_index_entry(pubkey, None, txn, old_alloc.offset)?;

        Ok(old_alloc)
    }

    /// Removes an account from all indices and marks its space as recyclable.
    pub(crate) fn remove(
        &self,
        pubkey: &Pubkey,
        txn: &mut RwTransaction<'_>,
    ) -> AccountsDbResult<()> {
        // Get allocation to know what to free
        let allocation = match self.get_allocation(txn, pubkey) {
            Ok(a) => a,
            Err(AccountsDbError::NotFound) => return Ok(()), // Idempotent
            Err(e) => return Err(e),
        };

        // Remove from Main Index
        self.accounts.remove(txn, pubkey, None)?;

        // Add to Deallocations Index
        let key = allocation.blocks.to_le_bytes();
        let val =
            bytes!(#pack, allocation.offset, Offset, allocation.blocks, Blocks);
        self.deallocations.upsert(txn, key, val)?;

        // Remove from Secondary Indices
        self.remove_program_index_entry(pubkey, None, txn, allocation.offset)?;
        self.owners.remove(txn, pubkey, None)?;

        Ok(())
    }

    /// Reconciles the `owners` and `programs` indices if the account's owner changed.
    pub(crate) fn ensure_correct_owner(
        &self,
        pubkey: &Pubkey,
        new_owner: &Pubkey,
        txn: &mut RwTransaction,
    ) -> AccountsDbResult<()> {
        let owner_bytes = self.owners.get(txn, pubkey)?;

        let old_owner = owner_bytes.and_then(|b| Pubkey::try_from(b).ok());

        let allocation = self.get_allocation(txn, pubkey)?;

        // 1. Clean up old program index entry
        self.remove_program_index_entry(
            pubkey,
            old_owner,
            txn,
            allocation.offset,
        )?;

        // 2. Insert new program index entry
        let offset_and_pubkey =
            bytes!(#pack, allocation.offset, Offset, *pubkey, Pubkey);
        self.programs.upsert(txn, new_owner, offset_and_pubkey)?;

        // 3. Update owners index
        self.owners.upsert(txn, pubkey, new_owner)?;

        Ok(())
    }

    /// Internal helper to retrieve full allocation info (Offset + Size).
    fn get_allocation<T: Transaction>(
        &self,
        txn: &T,
        pubkey: &Pubkey,
    ) -> AccountsDbResult<ExistingAllocation> {
        let Some(slice) = self.accounts.get(txn, pubkey)? else {
            return Err(AccountsDbError::NotFound);
        };
        let (offset, blocks) = bytes!(#unpack, slice, Offset, Blocks);
        Ok(ExistingAllocation { offset, blocks })
    }

    /// Finds and removes a `programs` index entry.
    /// If `old_owner` is not provided, it is looked up in the `owners` index.
    fn remove_program_index_entry(
        &self,
        pubkey: &Pubkey,
        old_owner: Option<Pubkey>,
        txn: &mut RwTransaction,
        offset: Offset,
    ) -> lmdb::Result<()> {
        // We need the owner to find the key in the programs index (Key=Owner).
        let owner = match old_owner {
            Some(pk) => pk,
            None => match self.owners.get(txn, pubkey)? {
                Some(val) => {
                    Pubkey::try_from(val).map_err(|_| lmdb::Error::Invalid)?
                }
                None => {
                    warn!(pubkey = %pubkey, "Account missing from owners index");
                    return Ok(());
                }
            },
        };

        // Value in programs index is {Offset, Pubkey}.
        // We need exact match to delete from DUPSORT db.
        let val = bytes!(#pack, offset, Offset, *pubkey, Pubkey);
        self.programs.remove(txn, owner, Some(&val))
    }

    /// Tries to find a free block of `needed` size in the deallocations table.
    /// If found, splits it if too large, and returns the recycled allocation.
    pub(crate) fn try_recycle_allocation(
        &self,
        needed: Blocks,
        txn: &mut RwTransaction,
    ) -> AccountsDbResult<ExistingAllocation> {
        let mut cursor = self.deallocations.cursor_rw(txn)?;

        // MDB_SET_RANGE: Position cursor at first key >= `needed`.
        // This effectively implements a "Best Fit" (or "First Sufficient Fit") strategy.
        let (_key_bytes, val_bytes) = cursor.get(
            Some(&needed.to_le_bytes()),
            None,
            utils::MDB_SET_RANGE_OP,
        )?;

        let (offset, available_blocks) =
            bytes!(#unpack, val_bytes, Offset, Blocks);

        // Remove this allocation from free list
        cursor.del(lmdb::WriteFlags::empty())?;

        // If we found a block larger than needed, split it and put the remainder back.
        let remainder = available_blocks.saturating_sub(needed);
        if remainder > 0 {
            let new_hole_offset = offset.saturating_add(needed);

            let new_key = remainder.to_le_bytes();
            let new_val =
                bytes!(#pack, new_hole_offset, Offset, remainder, Blocks);

            drop(cursor);
            // Insert the remainder back into deallocations
            self.deallocations.upsert(txn, new_key, new_val)?;
        }

        Ok(ExistingAllocation {
            offset,
            blocks: needed,
        })
    }

    // --- Iterators & Accessors ---

    pub(crate) fn get_program_accounts_iter(
        &self,
        program: &Pubkey,
    ) -> AccountsDbResult<OffsetPubkeyIter<'_>> {
        let txn = self.env.begin_ro_txn()?;
        OffsetPubkeyIter::new(&self.programs, txn, Some(program))
    }

    pub(crate) fn get_all_accounts(
        &self,
    ) -> AccountsDbResult<OffsetPubkeyIter<'_>> {
        let txn = self.env.begin_ro_txn()?;
        OffsetPubkeyIter::new(&self.programs, txn, None)
    }

    pub(crate) fn offset_finder(
        &self,
    ) -> AccountsDbResult<AccountOffsetFinder<'_>> {
        let txn = self.env.begin_ro_txn()?;
        AccountOffsetFinder::new(&self.accounts, txn)
    }

    pub(crate) fn get_accounts_count(&self) -> usize {
        let Ok(txn) = self.env.begin_ro_txn() else {
            return 0;
        };
        self.owners.entries(&txn)
    }

    pub(crate) fn flush(&self) {
        let _ = self.env.sync(true).log_err(|| "main index flushing");
    }

    pub(crate) fn reload(&mut self, dbpath: &Path) -> AccountsDbResult<()> {
        const DEFAULT_SIZE: usize = 1024 * 1024;
        *self = Self::new(DEFAULT_SIZE, dbpath)?;
        Ok(())
    }

    pub(crate) fn rwtxn(&self) -> lmdb::Result<RwTransaction<'_>> {
        self.env.begin_rw_txn()
    }
}

pub(crate) mod iterator;
mod table;
#[cfg(test)]
mod tests;
pub(super) mod utils;
