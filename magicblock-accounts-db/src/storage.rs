use std::{
    fs::File,
    io::{self, Write},
    mem::size_of,
    path::Path,
    ptr::NonNull,
    sync::atomic::{AtomicU32, AtomicU64, Ordering},
};

use magicblock_config::config::{AccountsDbConfig, BlockSize};
use memmap2::MmapMut;
use solana_account::AccountSharedData;
use tracing::error;

use crate::{
    error::{AccountsDbError, LogErr},
    index::{Blocks, Offset},
    AccountsDbResult,
};

/// The reserved size in bytes at the beginning of the file for the `StorageHeader`.
/// This area is excluded from the data region used for account storage.
const METADATA_STORAGE_SIZE: usize = 256;

// Size calculation:
// write_cursor(8) + slot(8) + block_size(4) + capacity_blocks(4) + recycled_count(4) = 28 bytes used.
const METADATA_HEADER_USED: usize = 28;
const METADATA_PADDING_SIZE: usize =
    METADATA_STORAGE_SIZE - METADATA_HEADER_USED;

/// The standard filename for the accounts database file.
pub(crate) const ACCOUNTS_DB_FILENAME: &str = "accounts.db";

/// The persistent memory layout of the storage metadata.
///
/// This struct maps directly to the first `METADATA_STORAGE_SIZE` bytes of the
/// memory-mapped file. It uses atomic types to allow concurrent access/updates
/// from multiple threads without external locking.
#[repr(C)]
struct StorageHeader {
    /// The current write position (high-water mark) in blocks.
    /// This is an index: `actual_offset = write_cursor * block_size`.
    ///
    /// This acts as the "head" of the append-only log. Atomic fetch_add is used
    /// here to reserve space for new allocations.
    write_cursor: AtomicU64,

    /// The latest slot number that has been written to this storage.
    /// Used for snapshotting and recovery to know the age of the data.
    slot: AtomicU64,

    /// The size of a single block in bytes (e.g., 128, 256, 512).
    /// This is immutable once the database is created.
    block_size: u32,

    /// The total capacity of the database in blocks.
    /// derived from: `(file_size - METADATA_STORAGE_SIZE) / block_size`.
    capacity_blocks: u32,

    /// A counter of blocks that have been marked as dead/recycled.
    /// This is purely for metrics or fragmentation estimation; it does not
    /// automatically reclaim space (this is an append-only structure).
    recycled_count: AtomicU32,

    /// Padding to ensure the struct size exactly matches `METADATA_STORAGE_SIZE`
    /// and maintains alignment for future extensions.
    _padding: [u8; METADATA_PADDING_SIZE],
}

// Compile-time assertion to ensure the header definition matches the reserved space.
const _: () = assert!(size_of::<StorageHeader>() == METADATA_STORAGE_SIZE);
// Ensure 8-byte alignment for 64-bit atomics.
const _: () = assert!(size_of::<StorageHeader>().is_multiple_of(8));

/// A handle to the memory-mapped accounts database.
///
/// This struct provides safe wrappers around the raw memory map, handling
/// atomic allocation, bounds checking, and pointer arithmetic.
#[cfg_attr(test, derive(Debug))]
pub struct AccountsStorage {
    /// The underlying memory-mapped file.
    /// Kept alive here to ensure the memory region remains valid.
    mmap: MmapMut,

    /// A raw pointer to the start of the *data* segment (skipping the header).
    ///
    /// # Safety
    /// This pointer is derived from `mmap` and is guaranteed to be valid
    /// as long as `mmap` is alive.
    data_region: NonNull<u8>,

    /// A cached copy of `header.block_size` as a `usize`.
    /// Optimization to avoid reading from the mmap (potential cache miss/atomic read)
    /// on every allocation or read.
    block_size: usize,
}

impl AccountsStorage {
    /// Opens an existing accounts database or creates a new one if it doesn't exist.
    ///
    /// # Arguments
    /// * `config` - Configuration defining block size and initial file size.
    /// * `directory` - The directory path where `accounts.db` will be located.
    ///
    /// # Returns
    /// * `AccountsDbResult<Self>` - The initialized storage handle.
    pub(crate) fn new(
        config: &AccountsDbConfig,
        directory: &Path,
    ) -> AccountsDbResult<Self> {
        let db_path = directory.join(ACCOUNTS_DB_FILENAME);
        let exists = db_path.exists();

        let mut file = File::options()
            .create(true)
            .truncate(false)
            .write(true)
            .read(true)
            .open(&db_path)
            .log_err(|| {
                format!("opening accounts db file at {}", db_path.display())
            })?;

        // If the file is new or empty, we must initialize the header and resize it.
        if !exists || file.metadata()?.len() == 0 {
            initialize_db_file(&mut file, config).log_err(|| {
                format!("initializing new accounts db at {}", db_path.display())
            })?;
        } else {
            // If it exists, ensure it is at least the expected size from config.
            let target_size = calculate_file_size(config);
            ensure_file_size(&mut file, target_size as u64)?;
        }

        Self::map_file(file)
    }

    /// Internal helper to memory-map the file and validate the header.
    fn map_file(file: File) -> AccountsDbResult<Self> {
        // SAFETY:
        // We are the exclusive owner of the File object here.
        // We rely on the OS to ensure the mmap is valid.
        let mut mmap = unsafe { MmapMut::map_mut(&file) }?;

        if mmap.len() < METADATA_STORAGE_SIZE {
            return Err(AccountsDbError::Internal(
                "memory map length is less than metadata requirement".into(),
            ));
        }

        // Validate and fixup metadata.
        // We scope the mutable borrow of the header to this block.
        let block_size = {
            // SAFETY: We verified mmap.len() >= METADATA_STORAGE_SIZE above.
            // Casting the first bytes to StorageHeader is safe because StorageHeader is #[repr(C)].
            let header =
                unsafe { &mut *(mmap.as_mut_ptr() as *mut StorageHeader) };
            Self::validate_header(header)?;

            // Check if the file was resized (e.g. by external tool or config change)
            // and update capacity_blocks to reflect reality.
            let actual_capacity = (mmap.len() - METADATA_STORAGE_SIZE)
                / header.block_size as usize;

            if actual_capacity as u32 != header.capacity_blocks {
                header.capacity_blocks = actual_capacity as u32;
            }

            header.block_size as usize
        };

        // Calculate the pointer to the data region (just past the header).
        // SAFETY: We verified mmap.len() >= METADATA_STORAGE_SIZE.
        // The pointer arithmetic remains within the valid mmap region.
        let data_region = unsafe {
            NonNull::new_unchecked(mmap.as_mut_ptr().add(METADATA_STORAGE_SIZE))
        };

        Ok(Self {
            mmap,
            data_region,
            block_size,
        })
    }

    /// Validates the consistency of the storage header.
    fn validate_header(header: &StorageHeader) -> AccountsDbResult<()> {
        let block_size_valid = [
            BlockSize::Block128,
            BlockSize::Block256,
            BlockSize::Block512,
        ]
        .iter()
        .any(|&bs| bs as u32 == header.block_size);

        if header.capacity_blocks == 0 || !block_size_valid {
            error!(
                block_size = header.block_size,
                capacity_blocks = header.capacity_blocks,
                "AccountsDB corruption detected"
            );
            return Err(AccountsDbError::Internal(
                "AccountsDB file corrupted".into(),
            ));
        }
        Ok(())
    }

    /// Reserves space for a new allocation in the storage.
    ///
    /// This method is thread-safe and lock-free. It uses atomic arithmetic to
    /// advance the write cursor.
    ///
    /// # Arguments
    /// * `size_bytes` - The number of bytes required for the account data.
    ///
    /// # Returns
    /// * `Ok(Allocation)` - A handle containing the raw pointer and offset.
    /// * `Err` - If the database is full.
    pub(crate) fn allocate(
        &self,
        size_bytes: usize,
    ) -> AccountsDbResult<Allocation> {
        let blocks_needed = self.blocks_required(size_bytes) as u64;
        let header = self.header();

        // Atomic fetch_add reserves a range of blocks for this thread.
        // `Relaxed` ordering is sufficient because we only care about uniqueness
        // of the index, not synchronization with other memory operations yet.
        let start_index = header
            .write_cursor
            .fetch_add(blocks_needed, Ordering::Relaxed);

        let end_index = start_index as usize + blocks_needed as usize;
        let capacity = header.capacity_blocks as usize;

        // Check for overflow (Database Full).
        if end_index > capacity {
            // Note: We don't roll back the atomic here. The space is technically "leaked"
            // at the end of the file, but since the DB is full/unusable anyway, this is acceptable.
            return Err(AccountsDbError::Internal(format!(
                "Database full: required {} blocks, available {}",
                blocks_needed,
                capacity.saturating_sub(start_index as usize)
            )));
        }

        // Calculate the raw memory address for this allocation.
        // SAFETY: We validated `end_index <= capacity` above, so this pointer
        // is guaranteed to be within the mapped data region.
        let ptr = unsafe {
            self.data_region.add(start_index as usize * self.block_size)
        };

        Ok(Allocation {
            ptr,
            offset: start_index as Offset,
            blocks: blocks_needed as Blocks,
        })
    }

    /// Reads an account from the storage at the given offset.
    ///
    /// # Arguments
    /// * `offset` - The block index where the account starts.
    ///
    /// # Safety
    /// This assumes `offset` points to a valid, previously allocated account.
    /// The Index system ensures we only request valid offsets.
    #[inline(always)]
    pub(crate) fn read_account(&self, offset: Offset) -> AccountSharedData {
        let ptr = self.resolve_ptr(offset).as_ptr();
        // SAFETY:
        // 1. `resolve_ptr` ensures the pointer is within bounds if `offset` is valid.
        // 2. `deserialize_from_mmap` must handle potentially untrusted bytes safely.
        unsafe { AccountSharedData::deserialize_from_mmap(ptr) }.into()
    }

    /// Reconstructs an `Allocation` handle from a previously stored offset.
    ///
    /// Used when reusing an existing slot in the index (though this append-only
    /// implementation generally doesn't overwrite data in place, this supports
    /// designs that might).
    pub(crate) fn recycle(&self, recycled: ExistingAllocation) -> Allocation {
        let ptr = self.resolve_ptr(recycled.offset);
        Allocation {
            offset: recycled.offset,
            blocks: recycled.blocks,
            ptr,
        }
    }

    /// Translates an abstract `Offset` (block index) to a raw memory pointer.
    #[inline]
    pub fn resolve_ptr(&self, offset: Offset) -> NonNull<u8> {
        let offset_bytes = offset as usize * self.block_size;
        // SAFETY:
        // The caller is responsible for ensuring `offset` is within valid bounds.
        // In the context of `AccountsStorage`, offsets come from the Index which
        // tracks valid allocations.
        unsafe { self.data_region.add(offset_bytes) }
    }

    // --- Metadata Accessors ---

    /// Helper to get a reference to the header structure.
    #[inline(always)]
    fn header(&self) -> &StorageHeader {
        // SAFETY: `mmap` is guaranteed to be at least METADATA_STORAGE_SIZE large
        // and properly aligned for `StorageHeader`.
        unsafe { &*(self.mmap.as_ptr() as *const StorageHeader) }
    }

    pub(crate) fn slot(&self) -> u64 {
        self.header().slot.load(Ordering::Relaxed)
    }

    pub(crate) fn update_slot(&self, val: u64) {
        self.header().slot.store(val, Ordering::Relaxed)
    }

    pub(crate) fn inc_recycled_count(&self, val: Blocks) {
        self.header()
            .recycled_count
            .fetch_add(val, Ordering::Relaxed);
    }

    pub(crate) fn dec_recycled_count(&self, val: Blocks) {
        self.header()
            .recycled_count
            .fetch_sub(val, Ordering::Relaxed);
    }

    /// Calculates how many blocks are needed to store `size_bytes`.
    pub(crate) fn blocks_required(&self, size_bytes: usize) -> Blocks {
        size_bytes.div_ceil(self.block_size) as Blocks
    }

    /// Returns the slice of memory currently containing valid data (up to the write cursor).
    pub(crate) fn active_segment(&self) -> &[u8] {
        let cursor =
            self.header().write_cursor.load(Ordering::Relaxed) as usize;
        // Calculate length: Header Size + (Written Blocks * Block Size)
        let len = (cursor * self.block_size + METADATA_STORAGE_SIZE)
            .min(self.mmap.len());
        &self.mmap[..len]
    }

    /// Flushes changes to disk.
    pub(crate) fn flush(&self) {
        let _ = self
            .mmap
            .flush()
            .log_err(|| "failed to sync flush the mmap");
    }

    /// Reloads the database from a different path (used for snapshots).
    ///
    /// This drops the current mmap and opens a new one at `db_path`.
    pub(crate) fn reload(&mut self, db_path: &Path) -> AccountsDbResult<()> {
        let mut file = File::options()
            .write(true)
            .read(true)
            .open(db_path.join(ACCOUNTS_DB_FILENAME))
            .log_err(|| {
                format!("opening adb file for reload at {}", db_path.display())
            })?;

        ensure_file_size(&mut file, self.size_bytes())?;
        *self = Self::map_file(file)?;
        Ok(())
    }

    /// Returns the total expected size of the file in bytes (Header + Data).
    pub(crate) fn size_bytes(&self) -> u64 {
        (self.header().capacity_blocks as u64 * self.block_size as u64)
            + METADATA_STORAGE_SIZE as u64
    }

    pub(crate) fn block_size(&self) -> usize {
        self.block_size
    }
}

/// Initializes a fresh accounts database file with the header.
fn initialize_db_file(
    file: &mut File,
    config: &AccountsDbConfig,
) -> AccountsDbResult<()> {
    const MIN_DB_SIZE: usize = 16 * 1024 * 1024;
    if config.database_size < MIN_DB_SIZE {
        return Err(AccountsDbError::Internal(format!(
            "database file should be larger than {} bytes",
            MIN_DB_SIZE
        )));
    }

    let target_size = calculate_file_size(config);
    // Determine capacity based on file size minus header.
    let total_blocks =
        (target_size - METADATA_STORAGE_SIZE) / config.block_size as usize;

    ensure_file_size(file, target_size as u64)?;

    // Prepare the initial header state.
    let header = StorageHeader {
        write_cursor: AtomicU64::new(0),
        slot: AtomicU64::new(0),
        block_size: config.block_size as u32,
        capacity_blocks: total_blocks as u32,
        recycled_count: AtomicU32::new(0),
        _padding: [0u8; METADATA_PADDING_SIZE],
    };

    // Serialize the header directly to bytes.
    // SAFETY: StorageHeader is `repr(C)` and contains only POD (Plain Old Data) types.
    let header_bytes = unsafe {
        std::slice::from_raw_parts(
            &header as *const StorageHeader as *const u8,
            size_of::<StorageHeader>(),
        )
    };

    file.write_all(header_bytes)?;
    Ok(file.flush()?)
}

/// Grows the file to `size` if it is currently smaller.
fn ensure_file_size(file: &mut File, size: u64) -> io::Result<()> {
    if file.metadata()?.len() < size {
        file.set_len(size)?;
    }
    Ok(())
}

/// Calculates the target file size, ensuring alignment with block size.
fn calculate_file_size(config: &AccountsDbConfig) -> usize {
    let block_size = config.block_size as usize;
    // Align total size to block size + metadata
    let blocks = config.database_size.div_ceil(block_size);
    blocks * block_size + METADATA_STORAGE_SIZE
}

/// Represents a successful allocation within the storage.
#[derive(Clone, Copy)]
pub struct Allocation {
    /// Raw pointer to the start of the allocated memory.
    pub(crate) ptr: NonNull<u8>,
    /// The block index (offset) of this allocation.
    pub(crate) offset: Offset,
    /// The number of blocks reserved.
    pub(crate) blocks: Blocks,
}

/// A struct representing a previously known allocation.
/// Used for recycling or testing equality.
#[cfg_attr(test, derive(Debug, Eq, PartialEq))]
pub struct ExistingAllocation {
    /// The block index (offset) of this allocation.
    pub(crate) offset: Offset,
    /// The number of blocks reserved.
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
