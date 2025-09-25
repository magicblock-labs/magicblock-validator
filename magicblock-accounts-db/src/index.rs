use std::path::Path;

use iterator::OffsetPubkeyIter;
use lmdb::{
    Cursor, DatabaseFlags, Environment, RwTransaction, Transaction, WriteFlags,
};
use log::warn;
use solana_pubkey::Pubkey;
use table::Table;
use utils::*;

use crate::{
    error::AccountsDbError,
    log_err,
    storage::{Allocation, ExistingAllocation},
    AdbResult,
};

const WEMPTY: WriteFlags = WriteFlags::empty();

const ACCOUNTS_INDEX: &str = "accounts-idx";
const PROGRAMS_INDEX: &str = "programs-idx";
const DEALLOCATIONS_INDEX: &str = "deallocations-idx";
const OWNERS_INDEX: &str = "owners-idx";

/// LMDB Index manager
#[cfg_attr(test, derive(Debug))]
pub(crate) struct AccountsDbIndex {
    /// Accounts Index, used for searching accounts by offset in the main storage
    ///
    /// the key is the account's pubkey (32 bytes)
    /// the value is a concatenation of:
    /// 1. offset in the storage (4 bytes)
    /// 2. number of allocated blocks (4 bytes)
    accounts: Table,
    /// Programs Index, used to keep track of owner->accounts
    /// mapping, significantly speeds up program accounts retrieval
    ///
    /// the key is the owner's pubkey (32 bytes)
    /// the value is a concatenation of:
    /// 1. offset in the storage (4 bytes)
    /// 2. account pubkey (32 bytes)
    programs: Table,
    /// Deallocation Index, used to keep track of allocation size of deallocated
    /// accounts, this is further utilized when defragmentation is required, by
    /// matching new accounts' size and already present "holes" in the database
    ///
    /// the key is the allocation size in blocks (4 bytes)
    /// the value is a concatenation of:
    /// 1. offset in the storage (4 bytes)
    /// 2. number of allocated blocks (4 bytes)
    deallocations: Table,
    /// Index map from accounts' pubkeys to their current owners, the index is
    /// used primarily for cleanup purposes when owner change occures and we need
    /// to cleanup programs index, so that old owner -> account mapping doesn't dangle
    ///
    /// the key is the account's pubkey (32 bytes)
    /// the value is owner's pubkey (32 bytes)
    owners: Table,
    /// Common envorinment for all of the tables
    env: Environment,
}

/// Helper macro to pack(merge) two types into single buffer of similar
/// combined length or to unpack(unmerge) them back into original types
macro_rules! bytes {
    (#pack, $hi: expr, $t1: ty, $low: expr, $t2: ty) => {{
        const S1: usize = size_of::<$t1>();
        const S2: usize = size_of::<$t2>();
        let mut buffer = [0_u8; S1 + S2];
        let ptr = buffer.as_mut_ptr();
        // SAFETY:
        // we made sure that buffer contains exact space required by both writes
        unsafe { (ptr as *mut $t1).write_unaligned($hi) };
        unsafe { (ptr.add(S1) as *mut $t2).write_unaligned($low) };
        buffer
    }};
    (#unpack, $packed: expr,  $t1: ty, $t2: ty) => {{
        let ptr = $packed.as_ptr();
        const S1: usize = size_of::<$t1>();
        // SAFETY:
        // this macro branch is called on values previously packed by first branch
        // so we essentially undo the packing on buffer of valid length
        let t1 = unsafe { (ptr as *const $t1).read_unaligned() };
        let t2 = unsafe { (ptr.add(S1) as *const $t2).read_unaligned() };
        (t1, t2)
    }};
}

impl AccountsDbIndex {
    /// Creates new index manager for AccountsDB, by
    /// opening/creating necessary lmdb environments
    pub(crate) fn new(size: usize, directory: &Path) -> AdbResult<Self> {
        // create an environment for all the tables
        let env = lmdb_env(directory, size).inspect_err(log_err!(
            "main index env creation at {}",
            directory.display()
        ))?;
        let accounts =
            Table::new(&env, ACCOUNTS_INDEX, DatabaseFlags::empty())?;
        let programs = Table::new(
            &env,
            PROGRAMS_INDEX,
            DatabaseFlags::DUP_SORT | DatabaseFlags::DUP_FIXED,
        )?;
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

    /// Retrieve the offset at which account can be read from main storage
    #[inline(always)]
    pub(crate) fn get_account_offset(&self, pubkey: &Pubkey) -> AdbResult<u32> {
        let txn = self.env.begin_ro_txn()?;
        let Some(offset) = self.accounts.get(&txn, pubkey)? else {
            return Err(AccountsDbError::NotFound);
        };
        let offset =
            // SAFETY:
            // The accounts index stores two u32 values (offset and blocks)
            // serialized into 8 byte long slice. Here we are interested only in the first 4 bytes
            // (offset). The memory used by lmdb to store the serialization might not be u32
            // aligned, so we make use `read_unaligned`.
            //
            // We read the data stored by corresponding put in `insert_account`,
            // thus it should be of valid length and contain valid value
            unsafe { (offset.as_ptr() as *const u32).read_unaligned() };
        Ok(offset)
    }

    /// Retrieve the offset and the size (number of blocks) given account occupies
    fn get_allocation<T: Transaction>(
        &self,
        txn: &T,
        pubkey: &Pubkey,
    ) -> AdbResult<ExistingAllocation> {
        let Some(slice) = self.accounts.get(txn, pubkey)? else {
            return Err(AccountsDbError::NotFound);
        };
        let (offset, blocks) = bytes!(#unpack, slice, u32, u32);
        Ok(ExistingAllocation { offset, blocks })
    }

    /// Insert account's allocation information into various indices, if
    /// account is already present, necessary bookkeeping will take place
    pub(crate) fn insert_account(
        &self,
        pubkey: &Pubkey,
        owner: &Pubkey,
        allocation: Allocation,
    ) -> AdbResult<Option<ExistingAllocation>> {
        let Allocation { offset, blocks, .. } = allocation;

        let mut txn = self.env.begin_rw_txn()?;
        let mut dealloc = None;

        // merge offset and block count into one single u64 and cast it to [u8; 8]
        let index_value = bytes!(#pack, offset, u32, blocks, u32);
        // concatenate offset where account is stored with pubkey of that account
        let offset_and_pubkey = bytes!(#pack, offset, u32, *pubkey, Pubkey);

        // optimisitically try to insert account to index, assuming that it doesn't exist
        let inserted =
            self.accounts
                .put_if_not_exists(&mut txn, pubkey, index_value)?;
        // if the account does exist, then it already occupies space in main storage
        if !inserted {
            // in which case we just move the account to a new allocation
            // adjusting all of the offsets and cleaning up the older ones
            let previous =
                self.reallocate_account(pubkey, &mut txn, &index_value)?;
            dealloc.replace(previous);
        };

        // track the account via programs' index as well
        self.programs.put(&mut txn, owner, offset_and_pubkey)?;
        // track the reverse relation between account and its owner
        self.owners.put(&mut txn, pubkey, owner)?;

        txn.commit()?;
        Ok(dealloc)
    }

    /// Helper method to change the allocation for a given account
    fn reallocate_account(
        &self,
        pubkey: &Pubkey,
        txn: &mut RwTransaction,
        index_value: &[u8],
    ) -> AdbResult<ExistingAllocation> {
        // retrieve the size and offset for allocation
        let allocation = self.get_allocation(txn, pubkey)?;
        // and put it into deallocation index, so the space can be recycled later
        let key = allocation.blocks.to_le_bytes();
        let value =
            bytes!(#pack, allocation.offset, u32, allocation.blocks, u32);
        self.deallocations.put(txn, key, value)?;

        // now we can overwrite the index record
        self.accounts.put(txn, pubkey, index_value)?;

        // we also need to delete old entry from `programs` index
        self.remove_programs_index_entry(pubkey, None, txn, allocation.offset)?;
        Ok(allocation)
    }

    /// Removes account from the database and marks its backing storage for recycling
    /// this method also performs various cleanup operations on the secondary indexes
    pub(crate) fn remove_account(&self, pubkey: &Pubkey) -> AdbResult<()> {
        let mut txn = self.env.begin_rw_txn()?;
        let mut cursor = self.accounts.cursor_rw(&mut txn)?;

        // locate the account entry
        let result = cursor
            .get(Some(pubkey.as_ref()), None, MDB_SET_OP)
            .map(|(_, v)| bytes!(#unpack, v, u32, u32));
        let (offset, blocks) = match result {
            Ok(r) => r,
            Err(lmdb::Error::NotFound) => return Ok(()),
            Err(err) => Err(err)?,
        };

        // and delete it
        cursor.del(WriteFlags::empty())?;
        drop(cursor);

        // mark the allocation for future recycling
        self.deallocations.put(
            &mut txn,
            blocks.to_le_bytes(),
            bytes!(#pack, offset, u32, blocks, u32),
        )?;

        // we also need to cleanup `programs` index
        self.remove_programs_index_entry(pubkey, None, &mut txn, offset)?;
        txn.commit()?;
        Ok(())
    }

    /// Ensures that current owner of account matches the one recorded in index,
    /// if not, the index cleanup will be performed and new entry inserted to
    /// match the current state
    pub(crate) fn ensure_correct_owner(
        &self,
        pubkey: &Pubkey,
        owner: &Pubkey,
    ) -> AdbResult<()> {
        let txn = self.env.begin_ro_txn()?;
        let old_owner = match self.owners.get(&txn, pubkey)? {
            // if current owner matches with that stored in index, then we are all set
            Some(val) if owner.as_ref() == val => {
                return Ok(());
            }
            None => return Ok(()),
            // if they don't match, then we have to remove old entries and create new ones
            Some(val) => Pubkey::try_from(val).ok(),
        };
        let allocation = self.get_allocation(&txn, pubkey)?;
        let mut txn = self.env.begin_rw_txn()?;
        // cleanup `programs` and `owners` index
        self.remove_programs_index_entry(
            pubkey,
            old_owner,
            &mut txn,
            allocation.offset,
        )?;
        // track new owner of the account via programs' index
        let offset_and_pubkey =
            bytes!(#pack, allocation.offset, u32, *pubkey, Pubkey);
        self.programs.put(&mut txn, owner, offset_and_pubkey)?;
        // track the reverse relation between account and its owner
        self.owners.put(&mut txn, pubkey, owner)?;

        txn.commit().map_err(Into::into)
    }

    fn remove_programs_index_entry(
        &self,
        pubkey: &Pubkey,
        old_owner: Option<Pubkey>,
        txn: &mut RwTransaction,
        offset: u32,
    ) -> lmdb::Result<()> {
        let val = bytes!(#pack, offset, u32, *pubkey, Pubkey);
        if let Some(owner) = old_owner {
            return self.programs.del(txn, owner, Some(&val));
        }
        // in order to delete the old entry from `programs` index, we consult
        // the `owners` index to fetch the previous owner of the account
        let mut owners = self.owners.cursor_rw(txn)?;
        let owner = match owners.get(Some(pubkey.as_ref()), None, MDB_SET_OP) {
            Ok((_, val)) => {
                let pk = Pubkey::try_from(val).inspect_err(log_err!(
                    "owners index contained invalid value for pubkey of len {}",
                    val.len()
                ));
                let Ok(owner) = pk else {
                    return Ok(());
                };
                owner
            }
            Err(lmdb::Error::NotFound) => {
                warn!("account {pubkey} didn't have owners index entry");
                return Ok(());
            }
            Err(e) => {
                return Err(e);
            }
        };
        owners.del(WEMPTY)?;
        drop(owners);

        self.programs.del(txn, owner, Some(&val))?;
        Ok(())
    }

    /// Returns an iterator over offsets and pubkeys of accounts for given
    /// program offsets can be used to retrieve the account from storage
    pub(crate) fn get_program_accounts_iter(
        &self,
        program: &Pubkey,
    ) -> AdbResult<OffsetPubkeyIter<'_>> {
        let txn = self.env.begin_ro_txn()?;
        OffsetPubkeyIter::new(&self.programs, txn, Some(program))
    }

    /// Returns an iterator over offsets and pubkeys of all accounts in database
    /// offsets can be used further to retrieve the account from storage
    pub(crate) fn get_all_accounts(&self) -> AdbResult<OffsetPubkeyIter<'_>> {
        let txn = self.env.begin_ro_txn()?;
        OffsetPubkeyIter::new(&self.programs, txn, None)
    }

    /// Returns the number of accounts in the database
    pub(crate) fn get_accounts_count(&self) -> usize {
        let Ok(txn) = self.env.begin_ro_txn() else {
            warn!("failed to start transaction for stats retrieval");
            return 0;
        };
        self.owners.entries(&txn)
    }

    /// Check whether allocation of given size (in blocks) exists.
    /// These are the allocations which are leftovers from
    /// accounts' reallocations due to their resizing/removal
    pub(crate) fn try_recycle_allocation(
        &self,
        space: u32,
    ) -> AdbResult<ExistingAllocation> {
        let mut txn = self.env.begin_rw_txn()?;
        let mut cursor = self.deallocations.cursor_rw(&mut txn)?;
        // this is a neat lmdb trick where we can search for entry with matching
        // or greater key since we are interested in any allocation of at least
        // `blocks` size or greater, this works perfectly well for this case

        let (_, val) =
            cursor.get(Some(&space.to_le_bytes()), None, MDB_SET_RANGE_OP)?;

        let (offset, mut blocks) = bytes!(#unpack, val, u32, u32);
        // delete the allocation record from recycleable list
        cursor.del(WEMPTY)?;
        // check whether the found allocation contains more space than necessary
        let remainder = blocks.saturating_sub(space);
        if remainder > 0 {
            // split the allocation, to maximize the efficiency of block reuse
            blocks = space;
            let new_offset = offset.saturating_add(blocks);
            let index_value = bytes!(#pack, new_offset, u32, remainder, u32);
            cursor.put(&remainder.to_le_bytes(), &index_value, WEMPTY)?;
        }

        drop(cursor);
        txn.commit()?;

        Ok(ExistingAllocation { offset, blocks })
    }

    pub(crate) fn flush(&self) {
        // it's ok to ignore potential error here, as it will only happen if something
        // utterly terrible happened at OS level, in which case we most likely won't even
        // reach this code in any case there's no meaningful way to handle these errors
        let _ = self
            .env
            .sync(true)
            .inspect_err(log_err!("main index flushing"));
    }

    /// Reopen the index databases from a different directory at provided path
    ///
    /// NOTE: this is a very cheap operation, as fast as opening a few files
    pub(crate) fn reload(&mut self, dbpath: &Path) -> AdbResult<()> {
        // set it to default lmdb map size, it will be
        // ignored if smaller than currently occupied
        const DEFAULT_SIZE: usize = 1024 * 1024;
        *self = Self::new(DEFAULT_SIZE, dbpath)?;
        Ok(())
    }

    /// Returns the number of deallocations in the database
    #[cfg(test)]
    pub(crate) fn get_delloactions_count(&self) -> usize {
        let Ok(txn) = self.env.begin_ro_txn() else {
            warn!("failed to start transaction for stats retrieval");
            return 0;
        };
        self.deallocations.entries(&txn)
    }
}

pub(crate) mod iterator;
mod utils;
//mod standalone;
mod table;
#[cfg(test)]
mod tests;
