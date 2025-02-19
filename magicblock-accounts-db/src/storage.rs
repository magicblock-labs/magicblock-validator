use std::{
    fs::File,
    io::{self, Write},
    path::Path,
    sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering::*},
};

use memmap2::MmapMut;

use crate::{index::RecycledAllocation, AdbConfig, AdbResult};

pub(crate) struct Allocation {
    pub(crate) storage: *mut u8,
    pub(crate) offset: u32,
    pub(crate) blocks: u32,
}

/// Extra space in database storage file reserved for metadata
/// Currently most of it is unused, but still reserved for future extensions
const METADATA_STORAGE_SIZE: usize = 256;

pub(crate) struct AccountsStorage {
    meta: StorageMeta,
    /// a mutable pointer into memory mapped region
    store: *mut u8,
    /// underlying memory mapped region, but we cannot use it directly as Rust
    /// borrowing rules prevent us from mutably accessing it concurrently
    mmap: MmapMut,
}

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
    pub(crate) fn new(config: &AdbConfig) -> AdbResult<Self> {
        let mut file = inspecterr!(
            File::options()
                .create(true)
                .truncate(false)
                .write(true)
                .read(true)
                .open(config.directory.join("accounts.db")),
            "opening adb file"
        );

        if file.metadata()?.len() == 0 {
            // database is being created for the first time, resize the file and write metadata
            inspecterr!(
                StorageMeta::init_adb_file(&mut file, config),
                "initializing new adb"
            );
        }

        let mut mmap = unsafe { MmapMut::map_mut(&file) }?;
        let meta = StorageMeta::new(&mmap);
        let store = unsafe { mmap.as_mut_ptr().add(METADATA_STORAGE_SIZE) };
        Ok(Self { mmap, meta, store })
    }

    pub(crate) fn alloc(&self, size: usize) -> Allocation {
        let blocks = self.get_block_count(size) as usize;
        let mut cur = self.head().load(Acquire);
        let mut new = cur + blocks;
        // CAS loop to perform lock free concurrent allocation
        let head = self.head();
        // ..., don't ask why
        let cas = AtomicUsize::compare_exchange;
        while let Err(v) = cas(head, cur, new, Release, Acquire) {
            cur = v;
            new = cur + blocks;
        }
        // when CAS succeeds, the allocated region is ours exclusively
        let storage = unsafe { self.store.add(cur * self.block_size()) };
        Allocation {
            storage,
            offset: cur as u32,
            blocks: blocks as u32,
        }
    }

    pub(crate) fn recycle(&self, recycled: RecycledAllocation) -> Allocation {
        let offset = recycled.offset as usize * self.block_size();
        let storage = unsafe { self.store.add(offset) };
        Allocation {
            offset: recycled.offset,
            blocks: recycled.blocks,
            storage,
        }
    }

    pub(crate) fn offset(&self, offset: u32) -> *mut u8 {
        let offset = (offset * self.meta.block_size) as usize;
        unsafe { self.store.add(offset) }
    }

    pub(crate) fn get_slot(&self) -> u64 {
        unsafe { &*self.meta.slot }.load(Relaxed)
    }

    pub(crate) fn set_slot(&self, val: u64) {
        unsafe { &*self.meta.slot }.store(val, Relaxed)
    }
    // TODO(bmuddha): use it to trigger recycling of freed blocks
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
        let blocks = size / block_size + (size % block_size != 0) as usize;
        blocks as u32
    }

    pub(crate) fn flush(&self, sync: bool) {
        if sync {
            let _ = self.mmap.flush();
        } else {
            let _ = self.mmap.flush_async();
        }
    }

    /// Reopen database from a different directory
    ///
    /// NOTE: this is a very cheap operation, as fast as opening a file
    pub(crate) fn reload(&mut self, dbpath: &Path) -> AdbResult<()> {
        let file = inspecterr!(
            File::options()
                .write(true)
                .read(true)
                .open(dbpath.join("accounts.db")),
            "opening adb file"
        );

        let mut mmap = unsafe { MmapMut::map_mut(&file) }?;
        let meta = StorageMeta::new(&mmap);
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
        unsafe { &*self.meta.head }
    }

    fn deallocated(&self) -> &AtomicU32 {
        unsafe { &*self.meta.deallocated }
    }
}

/// NOTE!: any change in metadata format should be reflected here
impl StorageMeta {
    fn init_adb_file(file: &mut File, config: &AdbConfig) -> io::Result<()> {
        // query page size of host OS
        let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as usize;
        // make the database size a multiple of OS page size (rounding up),
        // and add one more block worth of space for metadata storage
        let page_num = (config.db_size / page_size)
            + (config.db_size % page_size != 0) as usize;
        let db_size = page_num * page_size + METADATA_STORAGE_SIZE;
        let total_blocks = db_size as u32 / config.block_size as u32;
        // set the length of file and zero out the space, might take a while on huge files
        file.set_len(db_size as u64)?;

        // the storage itself starts immediately after metadata section
        let head = METADATA_STORAGE_SIZE / config.block_size as usize;
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

        file.flush()
    }

    fn new(store: &MmapMut) -> Self {
        const SLOT_OFFSET: usize = size_of::<usize>();
        const BLOCKSIZE_OFFSET: usize = SLOT_OFFSET + size_of::<u64>();
        const TOTALBLOCKS_OFFSET: usize = BLOCKSIZE_OFFSET + size_of::<u32>();
        const DEALLOCATED_OFFSET: usize = TOTALBLOCKS_OFFSET + size_of::<u32>();

        let ptr = store.as_ptr();

        // first element is head
        let head = ptr as *const AtomicUsize;
        // second element is slot
        let slot = unsafe { ptr.add(SLOT_OFFSET) as *const AtomicU64 };
        // third is blocks size
        let block_size =
            unsafe { (ptr.add(BLOCKSIZE_OFFSET) as *const u32).read() };
        // fourth is total blocks count
        let total_blocks =
            unsafe { (ptr.add(TOTALBLOCKS_OFFSET) as *const u32).read() };
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
