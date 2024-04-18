use byteorder::{BigEndian, ByteOrder};
use serde::{de::DeserializeOwned, Serialize};
use solana_sdk::clock::Slot;

use super::meta;

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

// When adding a new column ...
// - Add struct below and implement `Column` and `ColumnName` traits
// - Add descriptor in Rocks::cf_descriptors() and name in Rocks::columns()
// - Account for column in both `run_purge_with_stats()` and
//   `compact_storage()` in ledger/src/blockstore/blockstore_purge.rs !!
// - Account for column in `analyze_storage()` in ledger-tool/src/main.rs

pub fn columns() -> Vec<&'static str> {
    vec![TransactionStatusIndex::NAME]
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
