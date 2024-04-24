use byteorder::{BigEndian, ByteOrder};
use serde::{de::DeserializeOwned, Serialize};
use solana_sdk::{clock::Slot, pubkey::Pubkey, signature::Signature};
use solana_storage_proto::convert::generated;

use super::meta;

/// Column family for Transaction Status
const TRANSACTION_STATUS_CF: &str = "transaction_status";
/// Column family for Address Signatures
const ADDRESS_SIGNATURES_CF: &str = "address_signatures";
/// Column family for Slot Signatures
const SLOT_SIGNATURES_CF: &str = "slot_signatures";
/// Column family for the Transaction Status Index.
/// This column family is used for tracking the active primary index for columns that for
/// query performance reasons should not be indexed by Slot.
const TRANSACTION_STATUS_INDEX_CF: &str = "transaction_status_index";
/// Column family for Blocktime
const BLOCKTIME_CF: &str = "blocktime";
/// Column family for Confirmed Transaction
const CONFIRMED_TRANSACTION_CF: &str = "confirmed_transaction";
/// Column family for TransactionMemos
const TRANSACTION_MEMOS_CF: &str = "transaction_memos";
/// Column family for Performance Samples
const PERF_SAMPLES_CF: &str = "perf_samples";

#[derive(Debug)]
/// The transaction status column
///
/// * index type: `(`[`Signature`]`, `[`Slot`])`
/// * value type: [`generated::TransactionStatusMeta`]
pub struct TransactionStatus;

#[derive(Debug)]
/// The address signatures column
///
/// * index type: `(`[`Pubkey`]`, `[`Slot`]`, u32, `[`Signature`]`)`
/// *                account addr,   slot,  tx index, tx signature
/// * value type: [`blockstore_meta::AddressSignatureMeta`]
pub struct AddressSignatures;

/// The slot + transaction index Signature column.
/// It mainly serves to quickly iterate over all signatures in a slot
/// and sort them by transaction index.
///
/// It is very similar to [AddressSignatures], except we can find signatures
/// for any transaction just by slot instead of sorted by account address.
/// This is needed respect before/until signature limits for
/// [crate::store::api::Store::get_confirmed_signatures_for_address] even if
/// the transaction of that signature did not include the address.
///
/// * index type: `(`[`Slot`]`, u32)`
/// *                 slot,  tx index
/// * value type: [`[`solana_sdk::signature::Signature`]`]
pub struct SlotSignatures;

#[derive(Debug)]
/// The transaction status index column.
///
/// * index type: `u64` (see [`SlotColumn`])
/// * value type: [`blockstore_meta::TransactionStatusIndexMeta`]
pub struct TransactionStatusIndex;

/// The block time column
///
/// * index type: `u64` (see [`SlotColumn`])
/// * value type: [`UnixTimestamp`]
pub struct Blocktime;

/// The transaction with status column
///
/// NOTE: this doesn't exist in the original solana validator
///       as there instructions come in as shreds and are pieced
///       together from them
///
/// * index type: `(`[`Signature`]`, `[`Slot`])`
/// * value type: [`generated::Transaction`]
pub struct Transaction;

/// The transaction memos column
///
/// * index type: [`Signature`]
/// * value type: [`String`]
pub struct TransactionMemos;

#[derive(Debug)]
/// The performance samples column
///
/// * index type: `u64` (see [`SlotColumn`])
/// * value type: [`blockstore_meta::PerfSample`]
pub struct PerfSamples;

// When adding a new column ...
// - Add struct below and implement `Column` and `ColumnName` traits
// - Add descriptor in Rocks::cf_descriptors() and name in Rocks::columns()
// - Account for column in both `run_purge_with_stats()` and
//   `compact_storage()` in ledger/src/blockstore/blockstore_purge.rs !!
// - Account for column in `analyze_storage()` in ledger-tool/src/main.rs

pub fn columns() -> Vec<&'static str> {
    vec![
        TransactionStatus::NAME,
        AddressSignatures::NAME,
        SlotSignatures::NAME,
        TransactionStatusIndex::NAME,
        Blocktime::NAME,
        Transaction::NAME,
        TransactionMemos::NAME,
        PerfSamples::NAME,
    ]
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

impl TypedColumn for AddressSignatures {
    type Type = meta::AddressSignatureMeta;
}

impl TypedColumn for SlotSignatures {
    type Type = Signature;
}

impl TypedColumn for TransactionStatusIndex {
    type Type = meta::TransactionStatusIndexMeta;
}

pub trait ProtobufColumn: Column {
    type Type: prost::Message + Default;
}

/// SlotColumn is a trait for slot-based column families.  Its index is
/// essentially Slot (or more generally speaking, has a 1:1 mapping to Slot).
///
/// The clean-up of any LedgerColumn that implements SlotColumn is managed by
/// `LedgerCleanupService`, which will periodically deprecate and purge
/// oldest entries that are older than the latest root in order to maintain the
/// configured --limit-ledger-size under the validator argument.
pub trait SlotColumn<Index = Slot> {}

impl<T: SlotColumn> Column for T {
    type Index = Slot;

    /// Converts a u64 Index to its RocksDB key.
    fn key(slot: u64) -> Vec<u8> {
        let mut key = vec![0; 8];
        BigEndian::write_u64(&mut key[..], slot);
        key
    }

    /// Converts a RocksDB key to its u64 Index.
    fn index(key: &[u8]) -> u64 {
        BigEndian::read_u64(&key[..8])
    }

    fn slot(index: Self::Index) -> Slot {
        index
    }

    /// Converts a Slot to its u64 Index.
    fn as_index(slot: Slot) -> u64 {
        slot
    }
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
// AddressSignatures
// -----------------
impl Column for AddressSignatures {
    type Index = (Pubkey, Slot, u32, Signature);

    fn key(
        (pubkey, slot, transaction_index, signature): Self::Index,
    ) -> Vec<u8> {
        let mut key = vec![0; Self::CURRENT_INDEX_LEN];
        key[0..32].copy_from_slice(&pubkey.as_ref()[0..32]);
        BigEndian::write_u64(&mut key[32..40], slot);
        BigEndian::write_u32(&mut key[40..44], transaction_index);
        key[44..108].copy_from_slice(&signature.as_ref()[0..64]);
        key
    }

    fn index(key: &[u8]) -> Self::Index {
        <AddressSignatures as ColumnIndexDeprecation>::index(key)
    }

    fn slot(index: Self::Index) -> Slot {
        index.1
    }

    // The AddressSignatures column is not keyed by slot so this method is meaningless
    // See Column::as_index() declaration for more details
    fn as_index(_index: u64) -> Self::Index {
        (Pubkey::default(), 0, 0, Signature::default())
    }
}
impl ColumnName for AddressSignatures {
    const NAME: &'static str = ADDRESS_SIGNATURES_CF;
}

impl ColumnIndexDeprecation for AddressSignatures {
    const DEPRECATED_INDEX_LEN: usize = 112;
    const CURRENT_INDEX_LEN: usize = 108;
    type DeprecatedIndex = (u64, Pubkey, Slot, Signature);

    fn deprecated_key(
        (primary_index, pubkey, slot, signature): Self::DeprecatedIndex,
    ) -> Vec<u8> {
        let mut key = vec![0; Self::DEPRECATED_INDEX_LEN];
        BigEndian::write_u64(&mut key[0..8], primary_index);
        key[8..40].clone_from_slice(&pubkey.as_ref()[0..32]);
        BigEndian::write_u64(&mut key[40..48], slot);
        key[48..112].clone_from_slice(&signature.as_ref()[0..64]);
        key
    }

    fn try_deprecated_index(
        key: &[u8],
    ) -> std::result::Result<Self::DeprecatedIndex, IndexError> {
        if key.len() != Self::DEPRECATED_INDEX_LEN {
            return Err(IndexError::UnpackError);
        }
        let primary_index = BigEndian::read_u64(&key[0..8]);
        let pubkey = Pubkey::try_from(&key[8..40]).unwrap();
        let slot = BigEndian::read_u64(&key[40..48]);
        let signature = Signature::try_from(&key[48..112]).unwrap();
        Ok((primary_index, pubkey, slot, signature))
    }

    fn try_current_index(
        key: &[u8],
    ) -> std::result::Result<Self::Index, IndexError> {
        if key.len() != Self::CURRENT_INDEX_LEN {
            return Err(IndexError::UnpackError);
        }
        let pubkey = Pubkey::try_from(&key[0..32]).unwrap();
        let slot = BigEndian::read_u64(&key[32..40]);
        let transaction_index = BigEndian::read_u32(&key[40..44]);
        let signature = Signature::try_from(&key[44..108]).unwrap();
        Ok((pubkey, slot, transaction_index, signature))
    }

    fn convert_index(deprecated_index: Self::DeprecatedIndex) -> Self::Index {
        let (_primary_index, pubkey, slot, signature) = deprecated_index;
        (pubkey, slot, 0, signature)
    }
}

// -----------------
// SlotSignatures
// -----------------
const SLOT_SIGNATURES_INDEX_LEN: usize = 8 + 4;
impl Column for SlotSignatures {
    type Index = (Slot, u32);

    fn key((slot, tx_idx): Self::Index) -> Vec<u8> {
        let mut key = vec![0; SLOT_SIGNATURES_INDEX_LEN];
        BigEndian::write_u64(&mut key[0..8], slot);
        BigEndian::write_u32(&mut key[8..12], tx_idx);
        key
    }

    fn index(key: &[u8]) -> Self::Index {
        <SlotSignatures as ColumnIndexDeprecation>::index(key)
    }

    fn slot(index: Self::Index) -> Slot {
        index.0
    }

    fn as_index(slot: u64) -> Self::Index {
        (slot, 0)
    }
}

impl ColumnName for SlotSignatures {
    const NAME: &'static str = SLOT_SIGNATURES_CF;
}

impl ColumnIndexDeprecation for SlotSignatures {
    const DEPRECATED_INDEX_LEN: usize = SLOT_SIGNATURES_INDEX_LEN + 8;
    const CURRENT_INDEX_LEN: usize = SLOT_SIGNATURES_INDEX_LEN;

    type DeprecatedIndex = (u64, Slot, u32);

    fn deprecated_key(
        (primary_index, slot, tx_idx): Self::DeprecatedIndex,
    ) -> Vec<u8> {
        let mut key = vec![0; Self::DEPRECATED_INDEX_LEN];
        BigEndian::write_u64(&mut key[0..8], primary_index);
        BigEndian::write_u64(&mut key[8..16], slot);
        BigEndian::write_u32(&mut key[16..20], tx_idx);
        key
    }

    fn try_deprecated_index(
        key: &[u8],
    ) -> std::result::Result<Self::DeprecatedIndex, IndexError> {
        if key.len() != Self::DEPRECATED_INDEX_LEN {
            return Err(IndexError::UnpackError);
        }
        let primary_index = BigEndian::read_u64(&key[0..8]);
        let slot = BigEndian::read_u64(&key[8..16]);
        let tx_idx = BigEndian::read_u32(&key[16..20]);
        Ok((primary_index, slot, tx_idx))
    }

    fn try_current_index(
        key: &[u8],
    ) -> std::result::Result<Self::Index, IndexError> {
        if key.len() != Self::CURRENT_INDEX_LEN {
            return Err(IndexError::UnpackError);
        }
        let slot = BigEndian::read_u64(&key[0..8]);
        let tx_idx = BigEndian::read_u32(&key[8..12]);
        Ok((slot, tx_idx))
    }

    fn convert_index(deprecated_index: Self::DeprecatedIndex) -> Self::Index {
        let (_primary_index, slot, tx_idx) = deprecated_index;
        (slot, tx_idx)
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

// -----------------
// Blocktime
// -----------------
impl SlotColumn for Blocktime {}
impl ColumnName for Blocktime {
    const NAME: &'static str = BLOCKTIME_CF;
}
impl TypedColumn for Blocktime {
    type Type = solana_sdk::clock::UnixTimestamp;
}

// -----------------
// Transaction
// -----------------
impl Column for Transaction {
    // Same key as TransactionStatus
    type Index = <TransactionStatus as Column>::Index;

    fn key((signature, slot): Self::Index) -> Vec<u8> {
        <TransactionStatus as Column>::key((signature, slot))
    }

    fn index(key: &[u8]) -> Self::Index {
        <TransactionStatus as Column>::index(key)
    }

    fn slot(index: Self::Index) -> Slot {
        <TransactionStatus as Column>::slot(index)
    }

    // Like TransactionStatus the ConfirmedTransactoin column is not keyed
    // by slot so this method is meaningless
    fn as_index(slot: Slot) -> Self::Index {
        <TransactionStatus as Column>::as_index(slot)
    }
}

impl ColumnName for Transaction {
    const NAME: &'static str = CONFIRMED_TRANSACTION_CF;
}

impl ProtobufColumn for Transaction {
    type Type = generated::Transaction;
}

// Even though it is deprecated it is needed to implement iter_current_index_filtered
impl ColumnIndexDeprecation for Transaction {
    // Same key as TransactionStatus
    type DeprecatedIndex =
        <TransactionStatus as ColumnIndexDeprecation>::DeprecatedIndex;

    const DEPRECATED_INDEX_LEN: usize =
        <TransactionStatus as ColumnIndexDeprecation>::DEPRECATED_INDEX_LEN;
    const CURRENT_INDEX_LEN: usize =
        <TransactionStatus as ColumnIndexDeprecation>::CURRENT_INDEX_LEN;

    fn deprecated_key(index: Self::DeprecatedIndex) -> Vec<u8> {
        <TransactionStatus as ColumnIndexDeprecation>::deprecated_key(index)
    }

    fn try_deprecated_index(
        key: &[u8],
    ) -> std::result::Result<Self::DeprecatedIndex, IndexError> {
        <TransactionStatus as ColumnIndexDeprecation>::try_deprecated_index(key)
    }

    fn try_current_index(
        key: &[u8],
    ) -> std::result::Result<Self::Index, IndexError> {
        <TransactionStatus as ColumnIndexDeprecation>::try_current_index(key)
    }

    fn convert_index(deprecated_index: Self::DeprecatedIndex) -> Self::Index {
        <TransactionStatus as ColumnIndexDeprecation>::convert_index(
            deprecated_index,
        )
    }
}

// -----------------
// TransactionMemos
// -----------------
impl TypedColumn for TransactionMemos {
    type Type = String;
}

impl Column for TransactionMemos {
    type Index = (Signature, Slot);

    fn key((signature, slot): Self::Index) -> Vec<u8> {
        let mut key = vec![0; Self::CURRENT_INDEX_LEN];
        key[0..64].copy_from_slice(&signature.as_ref()[0..64]);
        BigEndian::write_u64(&mut key[64..72], slot);
        key
    }

    fn index(key: &[u8]) -> Self::Index {
        <TransactionMemos as ColumnIndexDeprecation>::index(key)
    }

    fn slot(index: Self::Index) -> Slot {
        index.1
    }

    fn as_index(index: u64) -> Self::Index {
        (Signature::default(), index)
    }
}

impl ColumnName for TransactionMemos {
    const NAME: &'static str = TRANSACTION_MEMOS_CF;
}

impl ColumnIndexDeprecation for TransactionMemos {
    const DEPRECATED_INDEX_LEN: usize = 64;
    const CURRENT_INDEX_LEN: usize = 72;
    type DeprecatedIndex = Signature;

    fn deprecated_key(signature: Self::DeprecatedIndex) -> Vec<u8> {
        let mut key = vec![0; Self::DEPRECATED_INDEX_LEN];
        key[0..64].copy_from_slice(&signature.as_ref()[0..64]);
        key
    }

    fn try_deprecated_index(
        key: &[u8],
    ) -> std::result::Result<Self::DeprecatedIndex, IndexError> {
        Signature::try_from(&key[..64]).map_err(|_| IndexError::UnpackError)
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
        (deprecated_index, 0)
    }
}

// -----------------
// PerfSamples
// -----------------
impl SlotColumn for PerfSamples {}
impl ColumnName for PerfSamples {
    const NAME: &'static str = PERF_SAMPLES_CF;
}

// -----------------
// Column Configuration
// -----------------

// Returns true if the column family enables compression.
pub fn should_enable_compression<C: 'static + Column + ColumnName>() -> bool {
    C::NAME == TransactionStatus::NAME
}
