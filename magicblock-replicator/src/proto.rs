//! Protocol message types for replication.
//!
//! # Wire Format
//!
//! The enum variant index serves as an implicit type tag.

use async_nats::Subject;
use magicblock_core::Slot;
use serde::{Deserialize, Serialize};
use solana_hash::Hash;
use solana_transaction::versioned::VersionedTransaction;

use crate::nats::Subjects;

/// Ordinal position of a transaction within a slot.
pub type TxIndex = u32;

/// Top-level replication message envelope.
///
/// Variant order is part of the wire format - reordering breaks compatibility.
#[derive(Deserialize, Serialize, Clone, Debug)]
pub enum Message {
    /// Transaction executed at a specific slot position.
    Transaction(Transaction),
    /// Slot boundary with blockhash for confirmation.
    Block(Block),
    /// Periodic checkpoint for state verification.
    SuperBlock(SuperBlock),
}

impl Message {
    pub(crate) fn subject(&self) -> Subject {
        match self {
            Self::Transaction(_) => Subjects::transaction(),
            Self::Block(_) => Subjects::block(),
            Self::SuperBlock(_) => Subjects::superblock(),
        }
    }

    pub(crate) fn slot_and_index(&self) -> (Slot, TxIndex) {
        match self {
            Self::Transaction(txn) => (txn.slot, txn.index),
            Self::Block(block) => (block.slot, 0),
            Self::SuperBlock(superblock) => (superblock.slot, 0),
        }
    }
}

/// Transaction with slot context for ordered replay.
#[derive(Deserialize, Serialize, Clone, Debug)]
pub struct Transaction {
    /// Slot where the transaction was executed.
    pub slot: Slot,
    /// Ordinal position within the slot.
    pub index: TxIndex,
    /// Bincode-encoded `VersionedTransaction`.
    pub payload: Vec<u8>,
}

/// Slot boundary marker with blockhash.
#[derive(Deserialize, Serialize, Clone, Debug)]
pub struct Block {
    /// Slot number.
    pub slot: Slot,
    /// Blockhash for this slot.
    pub hash: Hash,
    /// Unix timestamp (seconds).
    pub timestamp: i64,
}

/// Periodic checkpoint for state verification and catch-up.
#[derive(Deserialize, Serialize, Clone, Debug)]
pub struct SuperBlock {
    pub slot: Slot,
    /// Total blocks processed.
    pub blocks: u64,
    /// Total transactions processed.
    pub transactions: u64,
    /// Rolling checksum for verification.
    pub checksum: u64,
}

impl Transaction {
    /// Deserializes the inner `VersionedTransaction`.
    pub fn decode(&self) -> bincode::Result<VersionedTransaction> {
        bincode::deserialize(&self.payload)
    }
}
