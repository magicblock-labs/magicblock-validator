use std::{fs, path::Path};

use lmdb::{
    Cursor, Database, DatabaseFlags, Environment, EnvironmentFlags, RoCursor,
    RoTransaction, RwTransaction, Transaction, WriteFlags,
};
use solana_pubkey::Pubkey;

use crate::{inspecterr, storage::Allocation, AccountsDbConfig, AdbResult};

const WEMPTY: WriteFlags = WriteFlags::empty();

// Below LMDB cursor operation consts, were copied since they are not exposed in the public API
// See https://github.com/mozilla/lmdb-rs/blob/946167603dd6806f3733e18f01a89cee21888468/lmdb-sys/src/bindings.rs#L158

#[doc = "Position at first key greater than or equal to specified key."]
const MDB_SET_RANGE_OP: u32 = 17;
#[doc = "Position at specified key"]
const MDB_SET_OP: u32 = 15;
#[doc = "Position at first key/data item"]
const MDB_FIRST_OP: u32 = 0;
#[doc = "Position at next data item"]
const MDB_NEXT_OP: u32 = 8;
#[doc = "Position at next data item of current key. Only for #MDB_DUPSORT"]
const MDB_NEXT_DUP_OP: u32 = 9;
#[doc = "Return key/data at current cursor position"]
const MDB_GET_CURRENT_OP: u32 = 4;
#[doc = "Position at key/data pair. Only for #MDB_DUPSORT"]
const MDB_GET_BOTH_OP: u32 = 2;

const ACCOUNTS_PATH: &str = "accounts";
const ACCOUNTS_INDEX: Option<&str> = Some("accounts-idx");
const PROGRAMS_INDEX: Option<&str> = Some("programs-idx");
const DEALLOCATIONS_PATH: &str = "deallocations";

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
    /// Common envorinment for accounts and programs databases
    env: Environment,
}

/// Helper macro to pack(merge) two types into single buffer of similar
/// combined length or to unpack(unmerge) them back into original types
macro_rules! bytes {
    (#pack, $hi: expr, $t1: ty, $low: expr, $t2: ty) => {{
        const S1: usize = size_of::<$t1>();
        const S2: usize = size_of::<$t2>();
        let mut buffer = [0; S1 + S2];
        let ptr = buffer.as_mut_ptr();
        // # Safety
        // we made sure that buffer contains exact space required by both writes
        unsafe { (ptr as *mut $t1).write_unaligned($hi) };
        unsafe { (ptr.add(S1) as *mut $t2).write_unaligned($low) };
        buffer
    }};
    (#unpack, $packed: expr,  $t1: ty, $t2: ty) => {{
        let ptr = $packed.as_ptr();
        const S1: usize = size_of::<$t1>();
        // # Safety
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
    pub(crate) fn new(config: &AccountsDbConfig) -> AdbResult<Self> {
        // create an environment for 2 databases: accounts and programs index
        let env = lmdb_env(
            ACCOUNTS_PATH,
            &config.directory,
            config.index_map_size,
            2,
        )
        .inspect_err(inspecterr!(
            "main index env creation at {}",
            config.directory.display()
        ))?;
        let accounts = env.create_db(ACCOUNTS_INDEX, DatabaseFlags::empty())?;
        let programs = env.create_db(
            PROGRAMS_INDEX,
            DatabaseFlags::DUP_SORT | DatabaseFlags::DUP_FIXED,
        )?;
        let deallocations = StandaloneIndex::new(
            DEALLOCATIONS_PATH,
            &config.directory,
            config.index_map_size,
            DatabaseFlags::DUP_SORT | DatabaseFlags::DUP_FIXED,
        )?;
        Ok(Self {
            accounts,
            programs,
            deallocations,
            env,
        })
    }

    /// Retrieve the offset at which account can be read from main storage
    pub(crate) fn get_account_offset(&self, pubkey: &Pubkey) -> AdbResult<u32> {
        let txn = self.env.begin_ro_txn()?;
        let offset = txn.get(self.accounts, pubkey)?;
        let offset =
            // # Safety
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
                let previous = self.reallocate_account(
                    pubkey,
                    &mut txn,
                    owner,
                    &index_value,
                )?;
                dealloc.replace(previous);
            }
            Err(other) => return Err(other.into()),
        };

        // track the account via programs' index as well
        txn.put(self.programs, owner, &offset_and_pubkey, WEMPTY)?;

        txn.commit()?;
        Ok(dealloc)
    }

    /// Helper method to change the allocation for a given account
    fn reallocate_account(
        &self,
        pubkey: &Pubkey,
        txn: &mut RwTransaction,
        owner: &Pubkey,
        index_value: &[u8],
    ) -> AdbResult<ExistingAllocation> {
        // retrieve the size and offset for allocation
        let allocation = self.get_allocation(txn, pubkey)?;
        // and put it into deallocation index, so the space can be recycled later
        //
        // NOTE: we use Big Endian here to enforce alphabetical ordering of keys
        self.deallocations.put(
            allocation.blocks.to_be_bytes(),
            bytes!(#pack, allocation.offset, u32, allocation.blocks, u32),
        )?;
        // we also need to delete old entry from programs index
        let mut cursor = txn.open_rw_cursor(self.programs)?;

        let program_index_val =
            &bytes!(#pack, allocation.offset, u32, *pubkey, Pubkey);
        // if we cannot locate owner/offset:pubkey combo,
        // that means that owner have changed
        let found = cursor
            .get(
                Some(owner.as_ref()),
                Some(program_index_val),
                MDB_GET_BOTH_OP,
            )
            .is_ok();
        if found {
            // NOTE: we don't use txn.del here because
            // it just segfaults, reason is unclear
            cursor.del(WEMPTY)?;
        }
        drop(cursor);

        // and finally overwrite the index record
        txn.put(self.accounts, pubkey, &index_value, WEMPTY)?;
        AdbResult::Ok(allocation)
    }

    /// Removes account from database and marks its backing storage for recycle
    pub(crate) fn remove_account(
        &self,
        pubkey: &Pubkey,
        owner: &Pubkey,
    ) -> AdbResult<()> {
        let mut txn = self.env.begin_rw_txn()?;
        let mut cursor = txn.open_rw_cursor(self.accounts)?;
        // if we cannot locate owner/offset:pubkey combo,
        // that means that owner have changed
        let (offset, blocks) = cursor
            .get(Some(pubkey.as_ref()), None, MDB_SET_OP)
            .map(|(_, v)| bytes!(#unpack, v, u32, u32))?;

        cursor.del(WriteFlags::empty())?;
        drop(cursor);

        // mark the allocation for future recycling
        //
        // NOTE: we use Big Endian here to enforce alphabetical ordering of keys
        self.deallocations.put(
            blocks.to_be_bytes(),
            bytes!(#pack, offset, u32, blocks, u32),
        )?;

        cursor = txn.open_rw_cursor(self.programs)?;
        // locate the owner/account combo and delete it
        if cursor
            .get(
                Some(owner.as_ref()),
                Some(&bytes!(#pack, offset, u32, *pubkey, Pubkey)),
                MDB_GET_BOTH_OP,
            )
            .is_ok()
        {
            cursor.del(WriteFlags::empty())?;
        }
        drop(cursor);

        txn.commit()?;

        Ok(())
    }

    /// Returns an iterator over offsets and pubkeys of accounts for given
    /// program offsets can be used to retrieve the account from storage
    pub(crate) fn get_program_accounts_iter(
        &self,
        program: &Pubkey,
    ) -> AdbResult<OffsetPubkeyIter<'_, MDB_SET_OP, MDB_NEXT_DUP_OP>> {
        let txn = self.env.begin_ro_txn()?;
        OffsetPubkeyIter::new(self.programs, txn, program)
    }

    /// Returns an iterator over offsets and pubkeys of all accounts in database
    /// offsets can be used further to retrieve the account from storage
    pub(crate) fn get_all_accounts(
        &self,
    ) -> AdbResult<OffsetPubkeyIter<'_, MDB_FIRST_OP, MDB_NEXT_OP>> {
        let txn = self.env.begin_ro_txn()?;
        OffsetPubkeyIter::new(self.programs, txn, &Pubkey::default()) // we don't care about pubkey
    }

    /// Check whether allocation of given size (in blocks) exists those
    /// allocations are leftovers from account movements due to resizing
    pub(crate) fn try_recycle_allocation(
        &self,
        space: u32,
    ) -> AdbResult<ExistingAllocation> {
        let mut txn = self.deallocations.rwtxn()?;
        let mut cursor = txn.open_rw_cursor(self.deallocations.db)?;
        // this is a neat lmdb trick where we can search for entry with matching
        // or greater key since we are interested in any allocation of at least
        // `blocks` size or greater, this works perfectly well for this case
        //
        // NOTE: we use Big Endian here to enforce alphabetical ordering of keys
        let (_, val) =
            cursor.get(Some(&space.to_be_bytes()), None, MDB_SET_RANGE_OP)?;

        let (offset, blocks) = bytes!(#unpack, val, u32, u32);
        // delete the allocation record from recycleable list
        cursor.del(WEMPTY)?;

        drop(cursor);
        txn.commit()?;

        Ok(ExistingAllocation { offset, blocks })
    }

    pub(crate) fn flush(&self) {
        let _ = self.env.sync(true);
        let _ = self.deallocations.env.sync(true);
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
                inspecterr!("main index env creation at {}", dbpath.display()),
            )?;
        let accounts = env.create_db(ACCOUNTS_INDEX, DatabaseFlags::empty())?;
        let programs = env.create_db(
            PROGRAMS_INDEX,
            DatabaseFlags::DUP_SORT | DatabaseFlags::DUP_FIXED,
        )?;
        let deallocations = StandaloneIndex::new(
            DEALLOCATIONS_PATH,
            dbpath,
            DEFAULT_SIZE,
            DatabaseFlags::DUP_SORT | DatabaseFlags::DUP_FIXED,
        )?;
        self.env = env;
        self.accounts = accounts;
        self.programs = programs;
        self.deallocations = deallocations;
        Ok(())
    }
}

struct StandaloneIndex {
    db: Database,
    env: Environment,
}

impl StandaloneIndex {
    fn new(
        name: &str,
        dbpath: &Path,
        size: usize,
        flags: DatabaseFlags,
    ) -> AdbResult<Self> {
        let env = lmdb_env(name, dbpath, size, 1).inspect_err(inspecterr!(
            "deallocation index creation at {}",
            dbpath.display()
        ))?;
        let db = env.create_db(None, flags)?;
        Ok(Self { env, db })
    }

    fn put(
        &self,
        key: impl AsRef<[u8]>,
        val: impl AsRef<[u8]>,
    ) -> lmdb::Result<()> {
        let mut txn = self.env.begin_rw_txn()?;
        txn.put(self.db, &key, &val, WEMPTY)?;
        txn.commit()
    }

    fn rwtxn(&self) -> lmdb::Result<RwTransaction> {
        self.env.begin_rw_txn()
    }
}

/// Iterator over pubkeys and offsets, where accounts
/// for those pubkeys can be found in database
///
/// S: Starting position operation, determines where to place cursor initially
/// N: Next position operation, determines where to move cursor next
pub(crate) struct OffsetPubkeyIter<'env, const S: u32, const N: u32> {
    _txn: RoTransaction<'env>,
    cursor: RoCursor<'env>,
    terminated: bool,
}

impl<'a, const S: u32, const N: u32> OffsetPubkeyIter<'a, S, N> {
    fn new(
        db: Database,
        txn: RoTransaction<'a>,
        pubkey: &Pubkey,
    ) -> AdbResult<Self> {
        let cursor = txn.open_ro_cursor(db)?;
        // # Safety
        // nasty/neat trick for lifetime erasure, but we are upholding
        // the rust's  ownership contracts by keeping txn around as well
        let cursor: RoCursor = unsafe { std::mem::transmute(cursor) };
        // jump to the first entry, key might be ignored depending on OP
        cursor.get(Some(pubkey.as_ref()), None, S)?;
        Ok(Self {
            _txn: txn,
            cursor,
            terminated: false,
        })
    }
}

impl<const S: u32, const N: u32> Iterator for OffsetPubkeyIter<'_, S, N> {
    type Item = (u32, Pubkey);
    fn next(&mut self) -> Option<Self::Item> {
        (!self.terminated).then_some(())?;
        match self.cursor.get(None, None, MDB_GET_CURRENT_OP) {
            Ok(entry) => {
                // advance the cursor,
                let advance = self.cursor.get(None, None, N);
                // if we move past the iterable range, NotFound will be
                // triggered by OP, and we can terminate the iteration
                if let Err(lmdb::Error::NotFound) = advance {
                    self.terminated = true;
                }
                Some(bytes!(#unpack, entry.1, u32, Pubkey))
            }
            Err(error) => {
                log::warn!("error advancing offset iterator cursor: {error}");
                None
            }
        }
    }
}

fn lmdb_env(
    name: &str,
    dir: &Path,
    size: usize,
    maxdb: u32,
) -> lmdb::Result<Environment> {
    let lmdb_env_flags: EnvironmentFlags =
        // allows to manually trigger flush syncs, but OS initiated flushes are somewhat beyond our control
        EnvironmentFlags::NO_SYNC
        // don't bother with copy on write and mutate the memory
        // directly, saves CPU cycles and memory access
        | EnvironmentFlags::WRITE_MAP
        // we never read uninit memory, so there's no point in paying for meminit
        | EnvironmentFlags::NO_MEM_INIT;

    let path = dir.join(name);
    let _ = fs::create_dir_all(&path);
    Environment::new()
        .set_map_size(size)
        .set_max_dbs(maxdb)
        .set_flags(lmdb_env_flags)
        .open_with_permissions(&path, 0o644)
}

pub(crate) struct ExistingAllocation {
    pub(crate) offset: u32,
    pub(crate) blocks: u32,
}
