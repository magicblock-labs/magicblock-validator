use byteorder::{BigEndian, ByteOrder};
use prost::Message;
use serde::{de::DeserializeOwned, Serialize};
use solana_sdk::{clock::Slot, signature::Signature};
use solana_storage_proto::convert::generated;

use super::meta;

/// Column family for Transaction Status
const TRANSACTION_STATUS_CF: &str = "transaction_status";
/// Column family for the Transaction Status Index.
/// This column family is used for tracking the active primary index for columns that for
/// query performance reasons should not be indexed by Slot.
const TRANSACTION_STATUS_INDEX_CF: &str = "transaction_status_index";

// TODO(thlorenz): @@@blockstore add items we need

#[derive(Debug)]
/// The transaction status index column.
///
/// * index type: `u64` (see [`SlotColumn`])
/// * value type: [`blockstore_meta::TransactionStatusIndexMeta`]
pub struct TransactionStatusIndex;

#[derive(Debug)]
/// The transaction status column
///
/// * index type: `(`[`Signature`]`, `[`Slot`])`
/// * value type: [`generated::TransactionStatusMeta`]
pub struct TransactionStatus;

// When adding a new column ...
// - Add struct below and implement `Column` and `ColumnName` traits
// - Add descriptor in Rocks::cf_descriptors() and name in Rocks::columns()
// - Account for column in both `run_purge_with_stats()` and
//   `compact_storage()` in ledger/src/blockstore/blockstore_purge.rs !!
// - Account for column in `analyze_storage()` in ledger-tool/src/main.rs

pub fn columns() -> Vec<&'static str> {
    vec![TransactionStatus::NAME, TransactionStatusIndex::NAME]
}

// -----------------
// Traits
// -----------------
pub trait Column {
    type Index;

    fn key(index: Self::Index) -> Vec<u8>;
    fn index(key: &[u8]) -> Self::Index;
    // This trait method is primarily used by `Database::delete_range_cf()`, and is therefore only
    // relevant for columns keyed by Slot: ie. SlotColumns and columns that feature a Slot as the
    // first item in the key.
    fn as_index(slot: Slot) -> Self::Index;
    fn slot(index: Self::Index) -> Slot;
}

pub trait ColumnName {
    const NAME: &'static str;
}

pub trait TypedColumn: Column {
    type Type: Serialize + DeserializeOwned;
}

impl TypedColumn for TransactionStatusIndex {
    type Type = meta::TransactionStatusIndexMeta;
}

pub trait ProtobufColumn: Column {
    type Type: prost::Message + Default;
}

// -----------------
// ColumnIndexDeprecation
// -----------------
pub enum IndexError {
    UnpackError,
}

/// Helper trait to transition primary indexes out from the columns that are using them.
pub trait ColumnIndexDeprecation: Column {
    const DEPRECATED_INDEX_LEN: usize;
    const CURRENT_INDEX_LEN: usize;
    type DeprecatedIndex;

    fn deprecated_key(index: Self::DeprecatedIndex) -> Vec<u8>;
    fn try_deprecated_index(
        key: &[u8],
    ) -> std::result::Result<Self::DeprecatedIndex, IndexError>;

    fn try_current_index(
        key: &[u8],
    ) -> std::result::Result<Self::Index, IndexError>;
    fn convert_index(deprecated_index: Self::DeprecatedIndex) -> Self::Index;

    fn index(key: &[u8]) -> Self::Index {
        if let Ok(index) = Self::try_current_index(key) {
            index
        } else if let Ok(index) = Self::try_deprecated_index(key) {
            Self::convert_index(index)
        } else {
            // Way back in the day, we broke the TransactionStatus column key. This fallback
            // preserves the existing logic for ancient keys, but realistically should never be
            // executed.
            Self::as_index(0)
        }
    }
}

// -----------------
// TransactionStatus
// -----------------
impl Column for TransactionStatus {
    type Index = (Signature, Slot);

    fn key((signature, slot): Self::Index) -> Vec<u8> {
        let mut key = vec![0; Self::CURRENT_INDEX_LEN];
        key[0..64].copy_from_slice(&signature.as_ref()[0..64]);
        BigEndian::write_u64(&mut key[64..72], slot);
        key
    }

    fn index(key: &[u8]) -> (Signature, Slot) {
        <TransactionStatus as ColumnIndexDeprecation>::index(key)
    }

    fn slot(index: Self::Index) -> Slot {
        index.1
    }

    // The TransactionStatus column is not keyed by slot so this method is meaningless
    // See Column::as_index() declaration for more details
    fn as_index(_index: u64) -> Self::Index {
        (Signature::default(), 0)
    }
}

impl ColumnName for TransactionStatus {
    const NAME: &'static str = TRANSACTION_STATUS_CF;
}
impl ProtobufColumn for TransactionStatus {
    type Type = generated::TransactionStatusMeta;
}

impl ColumnIndexDeprecation for TransactionStatus {
    const DEPRECATED_INDEX_LEN: usize = 80;
    const CURRENT_INDEX_LEN: usize = 72;
    type DeprecatedIndex = (u64, Signature, Slot);

    fn deprecated_key(
        (index, signature, slot): Self::DeprecatedIndex,
    ) -> Vec<u8> {
        let mut key = vec![0; Self::DEPRECATED_INDEX_LEN];
        BigEndian::write_u64(&mut key[0..8], index);
        key[8..72].copy_from_slice(&signature.as_ref()[0..64]);
        BigEndian::write_u64(&mut key[72..80], slot);
        key
    }

    fn try_deprecated_index(
        key: &[u8],
    ) -> std::result::Result<Self::DeprecatedIndex, IndexError> {
        if key.len() != Self::DEPRECATED_INDEX_LEN {
            return Err(IndexError::UnpackError);
        }
        let primary_index = BigEndian::read_u64(&key[0..8]);
        let signature = Signature::try_from(&key[8..72]).unwrap();
        let slot = BigEndian::read_u64(&key[72..80]);
        Ok((primary_index, signature, slot))
    }

    fn try_current_index(
        key: &[u8],
    ) -> std::result::Result<Self::Index, IndexError> {
        if key.len() != Self::CURRENT_INDEX_LEN {
            return Err(IndexError::UnpackError);
        }
        let signature = Signature::try_from(&key[0..64]).unwrap();
        let slot = BigEndian::read_u64(&key[64..72]);
        Ok((signature, slot))
    }

    fn convert_index(deprecated_index: Self::DeprecatedIndex) -> Self::Index {
        let (_primary_index, signature, slot) = deprecated_index;
        (signature, slot)
    }
}

// -----------------
// TransactionStatusIndex
// -----------------
impl Column for TransactionStatusIndex {
    type Index = u64;

    fn key(index: u64) -> Vec<u8> {
        let mut key = vec![0; 8];
        BigEndian::write_u64(&mut key[..], index);
        key
    }

    fn index(key: &[u8]) -> u64 {
        BigEndian::read_u64(&key[..8])
    }

    fn slot(_index: Self::Index) -> Slot {
        unimplemented!()
    }

    fn as_index(slot: u64) -> u64 {
        slot
    }
}
impl ColumnName for TransactionStatusIndex {
    const NAME: &'static str = TRANSACTION_STATUS_INDEX_CF;
}
