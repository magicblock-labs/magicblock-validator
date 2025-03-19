use std::{
    fs::File,
    io::Write,
    path::Path,
    sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering::*},
};

use memmap2::MmapMut;

use crate::{
    config::BlockSize, error::AccountsDbError, index::ExistingAllocation,
    inspecterr, AccountsDbConfig, AdbResult,
};

pub(crate) struct Allocation {
    pub(crate) storage: *mut u8,
    pub(crate) offset: u32,
    pub(crate) blocks: u32,
}

/// Extra space in database storage file reserved for metadata
/// Currently most of it is unused, but still reserved for future extensions
const METADATA_STORAGE_SIZE: usize = 256;
const ADB_FILE: &str = "accounts.db";

pub(crate) struct AccountsStorage {
    meta: StorageMeta,
    /// a mutable pointer into memory mapped region
    store: *mut u8,
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
/// Metadata layout:
/// 1. head (offset into storage): 8 bytes
/// 2. slot latest slot observed: 8 bytes
/// 3. block size: 4 bytes
/// 4. total block count: 4 bytes
/// 5. deallocated block count: 4 bytes
struct StorageMeta {
    /// offset into memory map, where next allocation will be served
    head: *const AtomicUsize,
    /// latest slot written to this account
    slot: *const AtomicU64,
    /// size of the block (atomic unit of allocation)
    block_size: u32,
    /// total number of blocks in database
    total_blocks: u32,
    /// blocks that were deallocated and now require defragmentation
    deallocated: *const AtomicU32,
}

impl AccountsStorage {
    /// Open (or create if doesn't exist) an accountsdb storage
    ///
    /// _Note_: passed config is ignored if the database
    /// file already exists at supplied path
    pub(crate) fn new(config: &AccountsDbConfig) -> AdbResult<Self> {
        let dbpath = config.directory.join(ADB_FILE);
        let mut file = File::options()
            .create(true)
            .truncate(false)
            .write(true)
            .read(true)
            .open(&dbpath)
            .inspect_err(inspecterr!(
                "opening adb file at {}",
                dbpath.display()
            ))?;

        if file.metadata()?.len() == 0 {
            // database is being created for the first time, resize the file and write metadata
            StorageMeta::init_adb_file(&mut file, config).inspect_err(
                inspecterr!(
                    "initializing new adb at {}",
                    config.directory.display()
                ),
            )?;
        }

        // # Safety
        // Only accountsdb from validator process is modifying the file contents
        // through memory map, so the contract of MmapMut is upheld
        let mut mmap = unsafe { MmapMut::map_mut(&file) }?;
        if mmap.len() <= METADATA_STORAGE_SIZE {
            return Err(AccountsDbError::Internal(
                "memory map length is less than metadata requirement",
            ));
        };

        let meta = StorageMeta::new(&mmap);
        // # Safety
        // StorageMeta::init_adb_file made sure that mmap is large enough to hold the metadata
        // so jumping to the end of that segment still lands us within the mmap region
        let store = unsafe { mmap.as_mut_ptr().add(METADATA_STORAGE_SIZE) };
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

        // Safety
        // we have validated above that we are within bounds of mmap
        let storage = unsafe { self.store.add(offset * self.block_size()) };
        Allocation {
            storage,
            offset: offset as u32,
            blocks: blocks as u32,
        }
    }

    pub(crate) fn recycle(&self, recycled: ExistingAllocation) -> Allocation {
        let offset = recycled.offset as usize * self.block_size();
        // # Safety
        // offset is calculated from existing allocation within the map, thus
        // jumping to that offset will land us somewhere within those bounds
        let storage = unsafe { self.store.add(offset) };
        Allocation {
            offset: recycled.offset,
            blocks: recycled.blocks,
            storage,
        }
    }

    pub(crate) fn offset(&self, offset: u32) -> *mut u8 {
        // # Safety
        // offset is calculated from existing allocation within the map, thus
        // jumping to that offset will land us somewhere within those bounds
        let offset = (offset * self.meta.block_size) as usize;
        unsafe { self.store.add(offset) }
    }

    pub(crate) fn get_slot(&self) -> u64 {
        // # Safety
        // slot points to memory mapped region holding the metadata,
        // including the latest slot, it's safe to dereference it
        unsafe { &*self.meta.slot }.load(Relaxed)
    }

    pub(crate) fn set_slot(&self, val: u64) {
        // # Safety
        // slot points to memory mapped region holding the metadata,
        // including the latest slot, it's safe to dereference it
        unsafe { &*self.meta.slot }.store(val, Relaxed)
    }

    // TODO(bmuddha): use it to trigger recycling of freed blocks,
    // currently recycling is always on, which might be expensive
    #[allow(unused)]
    pub(crate) fn fragmentation(&self) -> f32 {
        let deallocated = self.deallocated().load(Relaxed) as f32;
        let total = self.meta.total_blocks as f32;
        deallocated / total
    }

    pub(crate) fn increment_deallocations(&self, val: u32) {
        self.deallocated().fetch_add(val, Relaxed);
    }

    pub(crate) fn decrement_deallocations(&self, val: u32) {
        self.deallocated().fetch_sub(val, Relaxed);
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
                .inspect_err(inspecterr!("failed to sync flush the mmap"));
        } else {
            let _ = self
                .mmap
                .flush_async()
                .inspect_err(inspecterr!("failed to async flush the mmap"));
        }
    }

    /// Reopen database from a different directory
    ///
    /// NOTE: this is a very cheap operation, as fast as opening a file
    pub(crate) fn reload(&mut self, dbpath: &Path) -> AdbResult<()> {
        let file = File::options()
            .write(true)
            .read(true)
            .open(dbpath.join(ADB_FILE))
            .inspect_err(inspecterr!(
                "opening adb file from snapshot at {}",
                dbpath.display()
            ))?;

        // Only accountsdb from validator process is modifying the file contents
        // through memory map, so the contract of MmapMut is upheld
        let mut mmap = unsafe { MmapMut::map_mut(&file) }?;
        let meta = StorageMeta::new(&mmap);
        // # Safety
        // Snapshots are created from the same file used by primary memory mapped file
        // and it's already large enough to contain metadata and possibly some accounts
        // so jumping to the end of that segment still lands us within the mmap region
        let store = unsafe { mmap.as_mut_ptr().add(METADATA_STORAGE_SIZE) };
        self.mmap = mmap;
        self.meta = meta;
        self.store = store;
        Ok(())
    }

    /// total number of bytes occupied by storage
    pub(crate) fn size(&self) -> u64 {
        (self.meta.total_blocks * self.meta.block_size) as u64
            + METADATA_STORAGE_SIZE as u64
    }

    fn block_size(&self) -> usize {
        self.meta.block_size as usize
    }

    fn head(&self) -> &AtomicUsize {
        // # Safety
        // head points to memory mapped region holding the metadata,
        // including the latest offset, it's safe to dereference it
        unsafe { &*self.meta.head }
    }

    fn deallocated(&self) -> &AtomicU32 {
        // # Safety
        // deallocated points to memory mapped region holding the metadata,
        // including the number of deallocations, it's safe to dereference it
        unsafe { &*self.meta.deallocated }
    }
}

/// NOTE!: any change in metadata format should be reflected here
impl StorageMeta {
    fn init_adb_file(
        file: &mut File,
        config: &AccountsDbConfig,
    ) -> AdbResult<()> {
        assert!(config.db_size > 0, "database file cannot be of 0 length");
        // query page size of host OS
        // # Safety
        // we are calling C code here, which unsafe by default
        let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        if page_size == -1 {
            return Err(AccountsDbError::Internal(
                "failed to query OS page size",
            ));
        }
        let page_size = page_size as usize;
        // make the database size a multiple of OS page size (rounding up),
        // and add one more block worth of space for metadata storage
        let page_num = config.db_size.div_ceil(page_size);
        let db_size = (page_num + 1) * page_size; // + 1 for metadata
        let total_blocks = db_size as u32 / config.block_size as u32;
        // set the fixed length of file, cannot be grown afterwards
        file.set_len(db_size as u64)?;

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

        // # Safety
        // All pointer arithmethic operations are safe because they are
        // performed on metadata segement of backing MmapMut, which is
        // guarranteed to be large enough, due to Self::init_adb_file

        let ptr = store.as_ptr();

        // first element is head
        let head = ptr as *const AtomicUsize;
        // second element is slot
        let slot = unsafe { ptr.add(SLOT_OFFSET) as *const AtomicU64 };
        // third is blocks size
        let block_size =
            unsafe { (ptr.add(BLOCKSIZE_OFFSET) as *const u32).read() };

        let initialized_block_size = [
            BlockSize::Block128,
            BlockSize::Block256,
            BlockSize::Block512,
        ]
        .iter()
        .any(|&bs| bs as u32 == block_size);
        // fourth is total blocks count
        let total_blocks =
            unsafe { (ptr.add(TOTALBLOCKS_OFFSET) as *const u32).read() };

        if !(total_blocks != 0 && initialized_block_size) {
            eprintln!(
                "AccountsDB file is not initialized properly. Block Size - \
                {block_size} and Total Block Count is: {total_blocks}"
            );
            let _ = std::io::stdout().flush();
            std::process::exit(1);
        }
        // fifth is the number of deallocated blocks so far
        let deallocated =
            unsafe { ptr.add(DEALLOCATED_OFFSET) as *const AtomicU32 };

        Self {
            head,
            slot,
            block_size,
            total_blocks,
            deallocated,
        }
    }
}
