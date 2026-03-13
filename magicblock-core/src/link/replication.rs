//! Protocol message types for replication.

use serde::{Deserialize, Serialize};
use solana_hash::Hash;
use solana_transaction::versioned::VersionedTransaction;

use crate::{Slot, TransactionIndex};

/// Index for block boundary markers (TransactionIndex::MAX - 1).
/// Used to identify Block messages in slot/index comparisons.
pub const BLOCK_INDEX: TransactionIndex = TransactionIndex::MAX - 1;

/// Index for superblock checkpoint markers (TransactionIndex::MAX).
/// Used to identify SuperBlock messages in slot/index comparisons.
pub const SUPERBLOCK_INDEX: TransactionIndex = TransactionIndex::MAX;

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
    /// Returns the (slot, index) position of this message.
    /// Block and SuperBlock messages use sentinel index values.
    pub fn slot_and_index(&self) -> (Slot, TransactionIndex) {
        match self {
            Self::Transaction(tx) => (tx.slot, tx.index),
            Self::Block(block) => (block.slot, BLOCK_INDEX),
            Self::SuperBlock(superblock) => (superblock.slot, SUPERBLOCK_INDEX),
        }
    }
}

/// Transaction with slot context for ordered replay.
#[derive(Deserialize, Serialize, Clone, Debug)]
pub struct Transaction {
    /// Slot where the transaction was executed.
    pub slot: Slot,
    /// Ordinal position within the slot.
    pub index: TransactionIndex,
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
    /// Rolling checksum for verification.
    pub checksum: u64,
}

impl Transaction {
    /// Deserializes the inner `VersionedTransaction`.
    pub fn decode(&self) -> bincode::Result<VersionedTransaction> {
        bincode::deserialize(&self.payload)
    }
}
