use std::path::Path;

use iterator::OffsetPubkeyIter;
use lmdb::{
    Cursor, Database, DatabaseFlags, Environment, RwTransaction, Transaction,
    WriteFlags,
};
use lmdb_utils::*;
use log::warn;
use solana_pubkey::Pubkey;
use standalone::StandaloneIndex;

use crate::{
    log_err,
    storage::{Allocation, ExistingAllocation},
    AccountsDbConfig, AdbResult,
};

const WEMPTY: WriteFlags = WriteFlags::empty();

const ACCOUNTS_PATH: &str = "accounts";
const ACCOUNTS_INDEX: Option<&str> = Some("accounts-idx");
const PROGRAMS_INDEX: Option<&str> = Some("programs-idx");
const DEALLOCATIONS_INDEX_PATH: &str = "deallocations";
const OWNERS_INDEX_PATH: &str = "owners";

/// LMDB Index manager
pub(crate) struct AccountsDbIndex {
    /// Accounts Index, used for searching accounts by offset in the main storage
    ///
    /// the key is the account's pubkey (32 bytes)
    /// the value is a concatenation of:
    /// 1. offset in the storage (4 bytes)
    /// 2. number of allocated blocks (4 bytes)
    accounts: Database,
    /// Programs Index, used to keep track of owner->accounts
    /// mapping, significantly speeds up program accounts retrieval
    ///
    /// the key is the owner's pubkey (32 bytes)
    /// the value is a concatenation of:
    /// 1. offset in the storage (4 bytes)
    /// 2. account pubkey (32 bytes)
    programs: Database,
    /// Deallocation Index, used to keep track of allocation size of deallocated
    /// accounts, this is further utilized when defragmentation is required, by
    /// matching new accounts' size and already present "holes" in database
    ///
    /// the key is the allocation size in blocks (4 bytes)
    /// the value is a concatenation of:
    /// 1. offset in the storage (4 bytes)
    /// 2. number of allocated blocks (4 bytes)
    deallocations: StandaloneIndex,
    /// Index map from accounts' pubkeys to their current owners, the index is
    /// used primarily for cleanup purposes when owner change occures and we need
    /// to cleanup programs index, so that old owner -> account mapping doesn't dangle
    ///
    /// the key is the account's pubkey (32 bytes)
    /// the value is owner's pubkey (32 bytes)
    owners: StandaloneIndex,
    /// Common envorinment for accounts and programs databases
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
    pub(crate) fn new(
        config: &AccountsDbConfig,
        directory: &Path,
    ) -> AdbResult<Self> {
        // create an environment for 2 databases: accounts and programs index
        let env = lmdb_env(ACCOUNTS_PATH, directory, config.index_map_size, 2)
            .inspect_err(log_err!(
                "main index env creation at {}",
                directory.display()
            ))?;
        let accounts = env.create_db(ACCOUNTS_INDEX, DatabaseFlags::empty())?;
        let programs = env.create_db(
            PROGRAMS_INDEX,
            DatabaseFlags::DUP_SORT | DatabaseFlags::DUP_FIXED,
        )?;
        let deallocations = StandaloneIndex::new(
            DEALLOCATIONS_INDEX_PATH,
            directory,
            config.index_map_size,
            DatabaseFlags::DUP_SORT | DatabaseFlags::DUP_FIXED,
        )?;

        let owners = StandaloneIndex::new(
            OWNERS_INDEX_PATH,
            directory,
            config.index_map_size,
            DatabaseFlags::empty(),
        )?;
        Ok(Self {
            accounts,
            programs,
            deallocations,
            env,
            owners,
        })
    }

    /// Retrieve the offset at which account can be read from main storage
    #[inline(always)]
    pub(crate) fn get_account_offset(&self, pubkey: &Pubkey) -> AdbResult<u32> {
        let txn = self.env.begin_ro_txn()?;
        let offset = txn.get(self.accounts, pubkey)?;
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
    fn get_allocation(
        &self,
        txn: &RwTransaction,
        pubkey: &Pubkey,
    ) -> AdbResult<ExistingAllocation> {
        let slice = txn.get(self.accounts, pubkey)?;
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
        let result = txn.put(
            self.accounts,
            pubkey,
            &index_value,
            WriteFlags::NO_OVERWRITE,
        );
        // if the account does exist, then it already occupies space in main storage
        match result {
            Ok(_) => {}
            // in which case we just move the account to new allocation
            // adjusting all offset and cleaning up older ones
            Err(lmdb::Error::KeyExist) => {
                let previous =
                    self.reallocate_account(pubkey, &mut txn, &index_value)?;
                dealloc.replace(previous);
            }
            Err(err) => return Err(err.into()),
        };

        // track the account via programs' index as well
        txn.put(self.programs, owner, &offset_and_pubkey, WEMPTY)?;
        // track the reverse relation between account and its owner
        self.owners.put(pubkey, owner)?;

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
        self.deallocations.put(
            BigEndianU32::new(allocation.blocks),
            bytes!(#pack, allocation.offset, u32, allocation.blocks, u32),
        )?;

        // now we can overwrite the index record
        txn.put(self.accounts, pubkey, &index_value, WEMPTY)?;

        // we also need to delete old entry from `programs` index
        match self.remove_programs_index_entry(pubkey, txn, allocation.offset) {
            Ok(()) | Err(lmdb::Error::NotFound) => Ok(allocation),
            Err(err) => Err(err.into()),
        }
    }

    /// Removes account from database and marks its backing storage for recycling
    /// this method also performs various cleanup operations on secondary indexes
    pub(crate) fn remove_account(&self, pubkey: &Pubkey) -> AdbResult<()> {
        let mut txn = self.env.begin_rw_txn()?;
        let mut cursor = txn.open_rw_cursor(self.accounts)?;

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
            BigEndianU32::new(blocks),
            bytes!(#pack, offset, u32, blocks, u32),
        )?;

        // we also need to cleanup `programs` index
        match self.remove_programs_index_entry(pubkey, &mut txn, offset) {
            Ok(()) | Err(lmdb::Error::NotFound) => {
                txn.commit()?;
            }
            Err(err) => return Err(err.into()),
        }
        Ok(())
    }

    /// Ensures that current owner of account matches the one recorded in index
    /// if not, the index cleanup will be performed and new entries inserted to
    /// match the current state
    pub(crate) fn ensure_correct_owner(
        &self,
        pubkey: &Pubkey,
        owner: &Pubkey,
    ) -> AdbResult<()> {
        match self.owners.getter()?.get(pubkey) {
            // if current owner matches with that stored in index, then we are all set
            Ok(val) if owner.as_ref() == val => {
                return Ok(());
            }
            Err(lmdb::Error::NotFound) => {
                return Ok(());
            }
            // if they don't match, well then we have to remove old entries and create new ones
            Ok(_) => (),
            Err(err) => Err(err)?,
        };
        let mut txn = self.env.begin_rw_txn()?;
        let allocation = self.get_allocation(&txn, pubkey)?;
        // cleanup `programs` and `owners` index
        self.remove_programs_index_entry(pubkey, &mut txn, allocation.offset)?;
        // track new owner of the account via programs' index
        let offset_and_pubkey =
            bytes!(#pack, allocation.offset, u32, *pubkey, Pubkey);
        txn.put(self.programs, owner, &offset_and_pubkey, WEMPTY)?;
        // track the reverse relation between account and its owner
        self.owners.put(pubkey, owner)?;

        txn.commit().map_err(Into::into)
    }

    fn remove_programs_index_entry(
        &self,
        pubkey: &Pubkey,
        txn: &mut RwTransaction,
        offset: u32,
    ) -> lmdb::Result<()> {
        // in order to delete old entry from `programs` index, we consult
        // `owners` index to fetch previous owner of the account
        let owner = match self.owners.getter()?.get(pubkey) {
            Ok(val) => {
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
            Err(err) => Err(err)?,
        };

        let mut cursor = txn.open_rw_cursor(self.programs)?;

        let key = Some(owner.as_ref());
        let val = bytes!(#pack, offset, u32, *pubkey, Pubkey);
        // locate the entry matching the owner and offset/pubkey combo
        let found = cursor.get(key, Some(&val), MDB_GET_BOTH_OP).is_ok();
        if found {
            // delete the entry only if it was located successfully
            cursor.del(WEMPTY)?;
        } else {
            // NOTE: this should never happend in consistent database
            warn!("account {pubkey} with owner {owner} didn't have programs index entry");
        }
        // and cleanup `owners` index as well
        self.owners.del(pubkey)?;
        Ok(())
    }

    /// Returns an iterator over offsets and pubkeys of accounts for given
    /// program offsets can be used to retrieve the account from storage
    pub(crate) fn get_program_accounts_iter(
        &self,
        program: &Pubkey,
    ) -> AdbResult<OffsetPubkeyIter<'_, MDB_SET_OP, MDB_NEXT_DUP_OP>> {
        let txn = self.env.begin_ro_txn()?;
        OffsetPubkeyIter::new(self.programs, txn, Some(program))
    }

    /// Returns an iterator over offsets and pubkeys of all accounts in database
    /// offsets can be used further to retrieve the account from storage
    pub(crate) fn get_all_accounts(
        &self,
    ) -> AdbResult<OffsetPubkeyIter<'_, MDB_FIRST_OP, MDB_NEXT_OP>> {
        let txn = self.env.begin_ro_txn()?;
        OffsetPubkeyIter::new(self.programs, txn, None)
    }

    /// Returns the number of accounts in database
    pub(crate) fn get_accounts_count(&self) -> usize {
        match self.env.stat() {
            Ok(stat) => stat.entries(),
            Err(err) => {
                warn!("Failed to get accounts count: {}", err);
                0
            }
        }
    }

    /// Check whether allocation of given size (in blocks) exists.
    /// These are the allocations which are leftovers from
    /// accounts' reallocations due to their resizing
    pub(crate) fn try_recycle_allocation(
        &self,
        space: u32,
    ) -> AdbResult<ExistingAllocation> {
        let mut cursor = self.deallocations.cursor()?;
        // this is a neat lmdb trick where we can search for entry with matching
        // or greater key since we are interested in any allocation of at least
        // `blocks` size or greater, this works perfectly well for this case

        let key = BigEndianU32::new(space);
        let (_, val) =
            cursor.get(Some(key.as_ref()), None, MDB_SET_RANGE_OP)?;

        let (offset, blocks) = bytes!(#unpack, val, u32, u32);
        // delete the allocation record from recycleable list
        cursor.del(WEMPTY)?;

        cursor.commit()?;

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
        self.deallocations.sync();
        self.owners.sync();
    }

    /// Reopen the index databases from a different directory at provided path
    ///
    /// NOTE: this is a very cheap operation, as fast as opening a few files
    pub(crate) fn reload(&mut self, dbpath: &Path) -> AdbResult<()> {
        // set it to default lmdb map size, it will be
        // ignored if smaller than currently occupied
        const DEFAULT_SIZE: usize = 1024 * 1024;
        let env =
            lmdb_env(ACCOUNTS_PATH, dbpath, DEFAULT_SIZE, 2).inspect_err(
                log_err!("main index env creation at {}", dbpath.display()),
            )?;
        let accounts = env.create_db(ACCOUNTS_INDEX, DatabaseFlags::empty())?;
        let programs = env.create_db(
            PROGRAMS_INDEX,
            DatabaseFlags::DUP_SORT | DatabaseFlags::DUP_FIXED,
        )?;
        let deallocations = StandaloneIndex::new(
            DEALLOCATIONS_INDEX_PATH,
            dbpath,
            DEFAULT_SIZE,
            DatabaseFlags::DUP_SORT | DatabaseFlags::DUP_FIXED,
        )?;
        let owners = StandaloneIndex::new(
            OWNERS_INDEX_PATH,
            dbpath,
            DEFAULT_SIZE,
            DatabaseFlags::empty(),
        )?;
        self.env = env;
        self.accounts = accounts;
        self.programs = programs;
        self.deallocations = deallocations;
        self.owners = owners;
        Ok(())
    }
}

pub(crate) mod iterator;
mod lmdb_utils;
mod standalone;
#[cfg(test)]
mod tests;
