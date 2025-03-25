use std::{
    fs::File,
    io::{self, Write},
    path::Path,
    ptr::NonNull,
    sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering::*},
};

use log::error;
use memmap2::MmapMut;
use solana_account::AccountSharedData;

use crate::{
    config::BlockSize, error::AccountsDbError, log_err, AccountsDbConfig,
    AdbResult,
};

/// Extra space in database storage file reserved for metadata
/// Currently most of it is unused, but still reserved for future extensions
const METADATA_STORAGE_SIZE: usize = 256;
pub(crate) const ADB_FILE: &str = "accounts.db";

pub(crate) struct AccountsStorage {
    meta: StorageMeta,
    /// a mutable pointer into memory mapped region
    store: NonNull<u8>,
    /// underlying memory mapped region, but we cannot use it directly as Rust
    /// borrowing rules prevent us from mutably accessing it concurrently
    mmap: MmapMut,
}

// TODO(bmuddha/tacopaco): use Unique pointer types
// from core::ptr once stable instead of raw pointers

/// Storage metadata manager
///
/// Metadata is persisted along with the actual accounts and is used to track various control
/// mechanisms of underlying storage
///
/// ---------------------------------------------------------
/// | Metadata In Memory Layout
/// ---------------------------------------------------------
/// | field         | description             | size in bytes
/// |---------------|-------------------------|--------------
/// | head          | offset into storage     | 8
/// | slot          | latest slot observed    | 8
/// | block size    | size of block           | 4
/// | total blocks  | total number of blocks  | 4
/// | deallocated   | deallocated block count | 4
/// ---------------------------------------------------------
struct StorageMeta {
    /// offset into memory map, where next allocation will be served
    head: &'static AtomicUsize,
    /// latest slot written to this account
    slot: &'static AtomicU64,
    /// size of the block (indivisible unit of allocation)
    block_size: u32,
    /// total number of blocks in database
    total_blocks: u32,
    /// blocks that were deallocated and now require defragmentation
    deallocated: &'static AtomicU32,
}

impl AccountsStorage {
    /// Open (or create if doesn't exist) an accountsdb storage
    ///
    /// _Note_: passed config is ignored if the database
    /// file already exists at supplied path
    pub(crate) fn new(
        config: &AccountsDbConfig,
        directory: &Path,
    ) -> AdbResult<Self> {
        let dbpath = directory.join(ADB_FILE);
        let mut file = File::options()
            .create(true)
            .truncate(false)
            .write(true)
            .read(true)
            .open(&dbpath)
            .inspect_err(log_err!(
                "opening adb file at {}",
                dbpath.display()
            ))?;

        if file.metadata()?.len() == 0 {
            // database is being created for the first time, resize the file and write metadata
            StorageMeta::init_adb_file(&mut file, config).inspect_err(
                log_err!("initializing new adb at {}", dbpath.display()),
            )?;
        }

        // SAFETY:
        // Only accountsdb from validator process is modifying the file contents
        // through memory map, so the contract of MmapMut is upheld
        let mut mmap = unsafe { MmapMut::map_mut(&file) }?;
        if mmap.len() <= METADATA_STORAGE_SIZE {
            return Err(AccountsDbError::Internal(
                "memory map length is less than metadata requirement",
            ));
        };

        let meta = StorageMeta::new(&mmap);
        // SAFETY:
        // StorageMeta::init_adb_file made sure that mmap is large enough to hold the metadata
        // so jumping to the end of that segment still lands us within the mmap region
        let store = unsafe {
            let pointer = mmap.as_mut_ptr().add(METADATA_STORAGE_SIZE);
            // as mmap points to non-null memory, pointer also points to non-null address
            NonNull::new_unchecked(pointer)
        };
        Ok(Self { mmap, meta, store })
    }

    pub(crate) fn alloc(&self, size: usize) -> Allocation {
        let blocks = self.get_block_count(size) as usize;

        let head = self.head();

        let offset = head.fetch_add(blocks, Release);

        // Ideally we should always have enough space to store accounts, 500 GB
        // should be enough to store every single account in solana and more,
        // but given that we operate on a tiny subset of that account pool, even
        // 10GB should be more than enough.
        //
        // Here we check that we haven't overflown the memory map and backing
        // file's size (and panic if we did), probably we need to implement
        // remapping with file growth, but considering that disk is limited,
        // this too can fail
        assert!(
            head.load(Relaxed) < self.meta.total_blocks as usize,
            "database is full"
        );

        // SAFETY:
        // we have validated above that we are within bounds of mmap and fetch_add
        // on head, reserved the offset number of blocks for our exclusive use
        let storage = unsafe { self.store.add(offset * self.block_size()) };
        Allocation {
            storage,
            offset: offset as u32,
            blocks: blocks as u32,
        }
    }

    #[inline(always)]
    pub(crate) fn read_account(&self, offset: u32) -> AccountSharedData {
        let memptr = self.offset(offset).as_ptr();
        // SAFETY:
        // offset is obtained from index and later transformed by storage (to translate to actual
        // address) always points to valid account allocation, as it's only possible to insert
        // something in database going in reverse, i.e. obtaining valid offset from storage
        // and then inserting it into index. So memory location pointed to memptr is valid.
        unsafe { AccountSharedData::deserialize_from_mmap(memptr) }.into()
    }

    pub(crate) fn recycle(&self, recycled: ExistingAllocation) -> Allocation {
        let offset = recycled.offset as usize * self.block_size();
        // SAFETY:
        // offset is calculated from existing allocation within the map, thus
        // jumping to that offset will land us somewhere within those bounds
        let storage = unsafe { self.store.add(offset) };
        Allocation {
            offset: recycled.offset,
            blocks: recycled.blocks,
            storage,
        }
    }

    pub(crate) fn offset(&self, offset: u32) -> NonNull<u8> {
        // SAFETY:
        // offset is calculated from existing allocation within the map, thus
        // jumping to that offset will land us somewhere within those bounds
        let offset = (offset * self.meta.block_size) as usize;
        unsafe { self.store.add(offset) }
    }

    pub(crate) fn get_slot(&self) -> u64 {
        self.meta.slot.load(Relaxed)
    }

    pub(crate) fn set_slot(&self, val: u64) {
        self.meta.slot.store(val, Relaxed)
    }

    pub(crate) fn increment_deallocations(&self, val: u32) {
        self.meta.deallocated.fetch_add(val, Relaxed);
    }

    pub(crate) fn decrement_deallocations(&self, val: u32) {
        self.meta.deallocated.fetch_sub(val, Relaxed);
    }

    pub(crate) fn get_block_count(&self, size: usize) -> u32 {
        let block_size = self.block_size();
        let blocks = size.div_ceil(block_size);
        blocks as u32
    }

    pub(crate) fn flush(&self, sync: bool) {
        if sync {
            let _ = self
                .mmap
                .flush()
                .inspect_err(log_err!("failed to sync flush the mmap"));
        } else {
            let _ = self
                .mmap
                .flush_async()
                .inspect_err(log_err!("failed to async flush the mmap"));
        }
    }

    /// Reopen database from a different directory
    ///
    /// NOTE: this is a very cheap operation, as fast as opening a file
    pub(crate) fn reload(&mut self, dbpath: &Path) -> AdbResult<()> {
        let mut file = File::options()
            .write(true)
            .read(true)
            .open(dbpath.join(ADB_FILE))
            .inspect_err(log_err!(
                "opening adb file from snapshot at {}",
                dbpath.display()
            ))?;
        // snapshot files are truncated, and contain only the actual data with no extra space to grow the
        // database, so we readjust the file's length to the preconfigured value before performing mmap
        adjust_database_file_size(&mut file, self.size())?;

        // Only accountsdb from validator process is modifying the file contents
        // through memory map, so the contract of MmapMut is upheld
        let mut mmap = unsafe { MmapMut::map_mut(&file) }?;
        let meta = StorageMeta::new(&mmap);
        // SAFETY:
        // Snapshots are created from the same file used by primary memory mapped file
        // and it's already large enough to contain metadata and possibly some accounts
        // so jumping to the end of that segment still lands us within the mmap region
        let store = unsafe {
            NonNull::new_unchecked(mmap.as_mut_ptr().add(METADATA_STORAGE_SIZE))
        };
        self.mmap = mmap;
        self.meta = meta;
        self.store = store;
        Ok(())
    }

    /// Returns the written segment (containing written data) of internal memory map
    pub(crate) fn mmap(&self) -> &[u8] {
        // get the last byte where data was written in storage segment and add the size
        // of metadata storage, this will give use used storage in backing file
        let mut end = self.meta.head.load(Relaxed) * self.block_size()
            + METADATA_STORAGE_SIZE;
        end = end.min(self.mmap.len());

        &self.mmap[..end]
    }

    /// total number of bytes occupied by storage
    pub(crate) fn size(&self) -> u64 {
        (self.meta.total_blocks * self.meta.block_size) as u64
            + METADATA_STORAGE_SIZE as u64
    }

    fn block_size(&self) -> usize {
        self.meta.block_size as usize
    }

    #[inline(always)]
    fn head(&self) -> &AtomicUsize {
        self.meta.head
    }
}

/// NOTE!: any change in metadata format should be reflected here
impl StorageMeta {
    fn init_adb_file(
        file: &mut File,
        config: &AccountsDbConfig,
    ) -> AdbResult<()> {
        assert!(config.db_size > 0, "database file cannot be of 0 length");
        let block_size = config.block_size as usize;
        let block_num = config.db_size.div_ceil(block_size);
        let meta_blocks = METADATA_STORAGE_SIZE.div_ceil(block_size);
        let db_size = (block_num + meta_blocks) * block_size;
        let total_blocks = db_size as u32 / block_size as u32;
        // grow the backing file as necessary
        adjust_database_file_size(file, db_size as u64)?;

        // the storage itself starts immediately after metadata section
        let head = 0_u64;
        file.write_all(&head.to_le_bytes())?;

        // fresh Accountsdb starts at slot 0
        let slot = 0_u64;
        file.write_all(&slot.to_le_bytes())?;

        // write blocksize
        file.write_all(&(config.block_size as u32).to_le_bytes())?;

        file.write_all(&total_blocks.to_le_bytes())?;
        // number of deallocated blocks, obviously 0 in a new database
        let deallocated = 0_u32;
        file.write_all(&deallocated.to_le_bytes())?;

        file.flush().map_err(Into::into)
    }

    fn new(store: &MmapMut) -> Self {
        const SLOT_OFFSET: usize = size_of::<u64>();
        const BLOCKSIZE_OFFSET: usize = SLOT_OFFSET + size_of::<u64>();
        const TOTALBLOCKS_OFFSET: usize = BLOCKSIZE_OFFSET + size_of::<u32>();
        const DEALLOCATED_OFFSET: usize = TOTALBLOCKS_OFFSET + size_of::<u32>();

        // SAFETY:
        // All pointer arithmethic operations are safe because they are
        // performed on metadata segment of backing MmapMut, which is
        // guarranteed to be large enough, due to Self::init_adb_file
        //
        // The pointer to static reference conversion is also sound as, memmap
        // is kept in the accountsdb during the entirety of its lifecycle

        let ptr = store.as_ptr();

        // first element is head
        let head = unsafe { &*(ptr as *const AtomicUsize) };
        // second element is slot
        let slot = unsafe { &*(ptr.add(SLOT_OFFSET) as *const AtomicU64) };
        // third is blocks size
        let block_size =
            unsafe { (ptr.add(BLOCKSIZE_OFFSET) as *const u32).read() };

        let block_size_is_initialized = [
            BlockSize::Block128,
            BlockSize::Block256,
            BlockSize::Block512,
        ]
        .iter()
        .any(|&bs| bs as u32 == block_size);
        // fourth is total blocks count
        let total_blocks =
            unsafe { (ptr.add(TOTALBLOCKS_OFFSET) as *const u32).read() };

        if !(total_blocks != 0 && block_size_is_initialized) {
            error!(
                "AccountsDB file is not initialized properly. Block Size - \
                {block_size} and Total Block Count is: {total_blocks}"
            );
            let _ = std::io::stdout().flush();
            std::process::exit(1);
        }
        // fifth is the number of deallocated blocks so far
        let deallocated =
            unsafe { &*(ptr.add(DEALLOCATED_OFFSET) as *const AtomicU32) };

        Self {
            head,
            slot,
            block_size,
            total_blocks,
            deallocated,
        }
    }
}

/// Helper function to grow the size of backing accounts db file
/// NOTE: this function cannot be use to shrink the file as the logic involved to
/// ensure that we don't accidentally truncate the written data is bit complex
fn adjust_database_file_size(file: &mut File, size: u64) -> io::Result<()> {
    if file.metadata()?.len() >= size {
        return Ok(());
    }
    file.set_len(size)
}

#[cfg_attr(test, derive(Clone, Copy))]
pub(crate) struct Allocation {
    pub(crate) storage: NonNull<u8>,
    pub(crate) offset: u32,
    pub(crate) blocks: u32,
}

#[cfg_attr(test, derive(Debug, Eq, PartialEq))]
pub(crate) struct ExistingAllocation {
    pub(crate) offset: u32,
    pub(crate) blocks: u32,
}

#[cfg(test)]
impl From<Allocation> for ExistingAllocation {
    fn from(value: Allocation) -> Self {
        Self {
            offset: value.offset,
            blocks: value.blocks,
        }
    }
}
