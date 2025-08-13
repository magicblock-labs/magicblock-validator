use std::{
    fs::File,
    io::{self, Write},
    path::Path,
    ptr::NonNull,
    sync::atomic::{AtomicU32, AtomicU64, Ordering::*},
};

use log::error;
use magicblock_config::{AccountsDbConfig, BlockSize};
use memmap2::MmapMut;
use solana_account::AccountSharedData;

use crate::{
    error::AccountsDbError,
    index::{Blocks, Offset},
    log_err, AccountsDbResult,
};

/// Extra space in database storage file reserved for metadata
/// Currently most of it is unused, but still reserved for future extensions
const METADATA_STORAGE_SIZE: usize = 256;
pub(crate) const ADB_FILE: &str = "accounts.db";

/// Different offsets into memory mapped file where various metadata fields are stored
const SLOT_OFFSET: usize = size_of::<u64>();
const BLOCKSIZE_OFFSET: usize = SLOT_OFFSET + size_of::<u64>();
const TOTALBLOCKS_OFFSET: usize = BLOCKSIZE_OFFSET + size_of::<u32>();
const DEALLOCATED_OFFSET: usize = TOTALBLOCKS_OFFSET + size_of::<u32>();

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
/// ----------------------------------------------------------
/// | Metadata In Memory Layout                              |
/// ----------------------------------------------------------
/// | field         | description             | size in bytes|
/// |---------------|-------------------------|---------------
/// | head          | offset into storage     | 8            |
/// | slot          | latest slot observed    | 8            |
/// | block size    | size of block           | 4            |
/// | total blocks  | total number of blocks  | 4            |
/// | deallocated   | deallocated block count | 4            |
/// ----------------------------------------------------------
struct StorageMeta {
    /// offset into memory map, where next allocation will be served
    head: &'static AtomicU64,
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
    /// NOTE:
    /// passed config is partially ignored if the database file already
    /// exists at the supplied path, for example, the size of main database
    /// file can be adjusted only up, the blocksize cannot be changed at all
    pub(crate) fn new(
        config: &AccountsDbConfig,
        directory: &Path,
    ) -> AccountsDbResult<Self> {
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
        } else {
            let db_size = calculate_db_size(config);
            adjust_database_file_size(&mut file, db_size as u64)?;
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

        let meta = StorageMeta::new(&mut mmap);
        // SAFETY:
        // StorageMeta::init_adb_file made sure that the mmap is large enough to hold the metadata,
        // so jumping to the end of that segment still lands us within the mmap region
        let store = unsafe {
            let pointer = mmap.as_mut_ptr().add(METADATA_STORAGE_SIZE);
            // as mmap points to non-null memory, the `pointer` also points to non-null address
            NonNull::new_unchecked(pointer)
        };
        Ok(Self { mmap, meta, store })
    }

    pub(crate) fn alloc(&self, size: usize) -> Allocation {
        let blocks = self.get_block_count(size) as u64;

        let head = self.head();

        let offset = head.fetch_add(blocks, Relaxed) as usize;

        // Ideally we should always have enough space to store accounts, 500 GB
        // should be enough to store every single account in solana and more,
        // but given that we operate on a tiny subset of that account pool, even
        // 10GB should be more than enough.
        //
        // Here we check that we haven't overflown the memory map and backing
        // file's size (and panic if we did), probably we need to implement
        // remapping with file growth, but considering that disk is limited,
        // this too can fail
        // https://github.com/magicblock-labs/magicblock-validator/issues/334
        assert!(offset < self.meta.total_blocks as usize, "database is full");

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
    pub(crate) fn read_account(&self, offset: Offset) -> AccountSharedData {
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

    pub(crate) fn offset(&self, offset: Offset) -> NonNull<u8> {
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

    pub(crate) fn increment_deallocations(&self, val: Blocks) {
        self.meta.deallocated.fetch_add(val, Relaxed);
    }

    pub(crate) fn decrement_deallocations(&self, val: Blocks) {
        self.meta.deallocated.fetch_sub(val, Relaxed);
    }

    pub(crate) fn get_block_count(&self, size: usize) -> Blocks {
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
    pub(crate) fn reload(&mut self, dbpath: &Path) -> AccountsDbResult<()> {
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

        // Only accountsdb from the validator process is modifying the file contents
        // through memory map, so the contract of MmapMut is upheld
        let mut mmap = unsafe { MmapMut::map_mut(&file) }?;
        let meta = StorageMeta::new(&mut mmap);
        // SAFETY:
        // Snapshots are created from the same file used by the primary memory mapped file
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

    /// Returns the utilized segment (containing written data) of internal memory map
    pub(crate) fn utilized_mmap(&self) -> &[u8] {
        // get the last byte where data was written in storage segment and add the size
        // of metadata storage, this will give us the used storage in backing file
        let head = self.meta.head.load(Relaxed) as usize;
        let mut end = head * self.block_size() + METADATA_STORAGE_SIZE;
        end = end.min(self.mmap.len());

        &self.mmap[..end]
    }

    /// total number of bytes occupied by storage
    pub(crate) fn size(&self) -> u64 {
        (self.meta.total_blocks as u64 * self.meta.block_size as u64)
            + METADATA_STORAGE_SIZE as u64
    }

    pub(crate) fn block_size(&self) -> usize {
        self.meta.block_size as usize
    }

    #[inline(always)]
    fn head(&self) -> &AtomicU64 {
        self.meta.head
    }
}

/// NOTE!: any change in metadata format should be reflected here
impl StorageMeta {
    fn init_adb_file(
        file: &mut File,
        config: &AccountsDbConfig,
    ) -> AccountsDbResult<()> {
        // Somewhat arbitrary min size for database, should be good enough for most test
        // cases, and prevent accidental creation of few kilobyte large or 0 sized databases
        const MIN_DB_SIZE: usize = 16 * 1024 * 1024;
        assert!(
            config.db_size > MIN_DB_SIZE,
            "database file should be larger than {MIN_DB_SIZE} bytes in length"
        );
        let db_size = calculate_db_size(config);
        let total_blocks = (db_size / config.block_size as usize) as Blocks;
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
        // number of deallocated blocks, obviously it's zero in a new database
        let deallocated = 0_u32;
        file.write_all(&deallocated.to_le_bytes())?;

        Ok(file.flush()?)
    }

    fn new(store: &mut MmapMut) -> Self {
        // SAFETY:
        // All pointer arithmethic operations are safe because they are performed
        // on the metadata segment of the backing MmapMut, which is guarranteed to
        // be large enough, due to previous call to Self::init_adb_file
        //
        // The pointer to static reference conversion is also sound, because the
        // memmap is kept in the accountsdb for the entirety of its lifecycle

        let ptr = store.as_mut_ptr();

        // first element is the head
        let head = unsafe { &*(ptr as *const AtomicU64) };
        // second element is the slot
        let slot = unsafe { &*(ptr.add(SLOT_OFFSET) as *const AtomicU64) };
        // third is the blocks size
        let block_size =
            unsafe { (ptr.add(BLOCKSIZE_OFFSET) as *const u32).read() };

        let block_size_is_initialized = [
            BlockSize::Block128,
            BlockSize::Block256,
            BlockSize::Block512,
        ]
        .iter()
        .any(|&bs| bs as u32 == block_size);
        // fourth is the total blocks count
        let mut total_blocks =
            unsafe { (ptr.add(TOTALBLOCKS_OFFSET) as *const u32).read() };
        // check whether the size of database file has been readjusted
        let adjusted_total_blocks =
            (store.len() / block_size as usize) as Blocks;
        if adjusted_total_blocks != total_blocks {
            // if so, use the adjusted number of total blocks
            total_blocks = adjusted_total_blocks;
            // and persist the new value to the disk via mmap
            // SAFETY:
            // we just read this value, above, and now we are just overwriting it with new 4 bytes
            unsafe {
                (ptr.add(TOTALBLOCKS_OFFSET) as *mut u32)
                    .write(adjusted_total_blocks)
            };
        }

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

/// Helper function to grow the size of the backing accounts db file
/// NOTE: this function cannot be used to shrink the file, as the logic involved to
/// ensure, that we don't accidentally truncate the written data, is a bit complex
fn adjust_database_file_size(file: &mut File, size: u64) -> io::Result<()> {
    if file.metadata()?.len() >= size {
        return Ok(());
    }
    file.set_len(size)
}

fn calculate_db_size(config: &AccountsDbConfig) -> usize {
    let block_size = config.block_size as usize;
    let block_num = config.db_size.div_ceil(block_size);
    let meta_blocks = METADATA_STORAGE_SIZE.div_ceil(block_size);
    (block_num + meta_blocks) * block_size
}

#[cfg_attr(test, derive(Clone, Copy))]
pub(crate) struct Allocation {
    pub(crate) storage: NonNull<u8>,
    pub(crate) offset: Offset,
    pub(crate) blocks: Blocks,
}

#[cfg_attr(test, derive(Debug, Eq, PartialEq))]
pub(crate) struct ExistingAllocation {
    pub(crate) offset: Offset,
    pub(crate) blocks: Blocks,
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
