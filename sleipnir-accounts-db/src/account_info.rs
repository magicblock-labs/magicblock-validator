use modular_bitfield::{bitfield, specifiers::B31};

use crate::{IsCached, ZeroLamport};

#[derive(Debug, PartialEq, Eq)]
pub enum StorageLocation {
    Cached,
}

// -----------------
// PackedOffsetAndFlags
// -----------------
#[bitfield(bits = 32)]
#[repr(C)]
#[derive(Debug, Default, Copy, Clone, Eq, PartialEq)]
pub struct PackedOffsetAndFlags {
    /// this provides 2^31 bits, which when multiplied by 8 (sizeof(u64)) = 16G, which is the maximum size of an append vec
    offset_reduced: B31,
    /// use 1 bit to specify that the entry is zero lamport
    is_zero_lamport: bool,
}
pub type AppendVecId = u32;

/// how large the offset we store in AccountInfo is
/// Note this is a smaller datatype than 'Offset'
/// AppendVecs store accounts aligned to u64, so offset is always a multiple of 8 (sizeof(u64))
pub type OffsetReduced = u32;

/// This is an illegal value for 'offset'.
/// Account size on disk would have to be pointing to the very last 8 byte value in the max sized append vec.
/// That would mean there was a maximum size of 8 bytes for the last entry in the append vec.
/// A pubkey alone is 32 bytes, so there is no way for a valid offset to be this high of a value.
/// Realistically, a max offset is (1<<31 - 156) bytes or so for an account with zero data length. Of course, this
/// depends on the layout on disk, compression, etc. But, 8 bytes per account will never be possible.
/// So, we use this last value as a sentinel to say that the account info refers to an entry in the write cache.
const CACHED_OFFSET: OffsetReduced = (1 << (OffsetReduced::BITS - 1)) - 1;
const CACHE_VIRTUAL_STORAGE_ID: AppendVecId = AppendVecId::MAX;

#[derive(Default, Debug, PartialEq, Eq, Clone, Copy)]
pub struct AccountOffsetAndFlags {
    /// offset = 'packed_offset_and_flags.offset_reduced()' * ALIGN_BOUNDARY_OFFSET into the storage
    /// Note this is a smaller type than 'Offset'
    packed_offset_and_flags: PackedOffsetAndFlags,
}

// -----------------
// AccountInfo
// -----------------
// NOTE: we keep this around even though we only store cached accounts at this point
// However this will be useful when we start storing accounts on disk as well
#[derive(Default, Debug, PartialEq, Eq, Clone, Copy)]
pub struct AccountInfo {
    /// index identifying the append storage
    store_id: AppendVecId,
    account_offset_and_flags: AccountOffsetAndFlags,
}

impl AccountInfo {
    pub fn new(storage_location: StorageLocation, lamports: u64) -> Self {
        let mut packed_offset_and_flags = PackedOffsetAndFlags::default();
        let store_id = match storage_location {
            StorageLocation::Cached => {
                packed_offset_and_flags.set_offset_reduced(CACHED_OFFSET);
                CACHE_VIRTUAL_STORAGE_ID
            }
        };

        packed_offset_and_flags.set_is_zero_lamport(lamports == 0);
        let account_offset_and_flags = AccountOffsetAndFlags {
            packed_offset_and_flags,
        };
        Self {
            store_id,
            account_offset_and_flags,
        }
    }
}

impl ZeroLamport for AccountInfo {
    fn is_zero_lamport(&self) -> bool {
        self.account_offset_and_flags
            .packed_offset_and_flags
            .is_zero_lamport()
    }
}

impl IsCached for AccountInfo {
    fn is_cached(&self) -> bool {
        self.account_offset_and_flags
            .packed_offset_and_flags
            .offset_reduced()
            == CACHED_OFFSET
    }
}
