//! Protocol message types for replication.
//!
//! Wire format: 4-byte LE length prefix + bincode payload.
//! Bincode encodes enum variant index as implicit type tag.

use serde::{Deserialize, Serialize};
use solana_hash::Hash;
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_signature::Signature;
use solana_signer::Signer;
use solana_transaction::versioned::VersionedTransaction;

pub type Slot = u64;
pub type TxIndex = u32;

pub const PROTOCOL_VERSION: u32 = 1;

/// Top-level replication message.
#[derive(Deserialize, Serialize, Clone, Debug)]
pub enum Message {
    Transaction(Transaction),
    Block(Block),
    SuperBlock(SuperBlock),
    Failover(FailoverSignal),
}

/// Slot boundary marker with blockhash.
#[derive(Deserialize, Serialize, Clone, Debug)]
pub struct Block {
    pub slot: Slot,
    pub hash: Hash,
    pub timestamp: i64,
}

/// Transaction with slot and ordinal position.
#[derive(Deserialize, Serialize, Clone, Debug)]
pub struct Transaction {
    pub slot: Slot,
    pub index: TxIndex,
    /// Bincode-encoded `VersionedTransaction`.
    pub payload: Vec<u8>,
}

/// Periodic checkpoint for state verification.
#[derive(Deserialize, Serialize, Clone, Debug)]
pub struct SuperBlock {
    pub blocks: u64,
    pub transactions: u64,
    pub checksum: u64,
}

/// Primary -> Standby: signal controlled failover.
#[derive(Deserialize, Serialize, Clone, Debug)]
pub struct FailoverSignal {
    pub slot: Slot,
    signature: Signature,
}

impl Transaction {
    /// Deserializes the inner transaction.
    pub fn decode(&self) -> bincode::Result<VersionedTransaction> {
        bincode::deserialize(&self.payload)
    }
}

impl FailoverSignal {
    pub fn new(slot: Slot, keypair: &Keypair) -> Self {
        Self {
            slot,
            signature: keypair.sign_message(&slot.to_le_bytes()),
        }
    }

    /// Verifies signal against expected identity.
    pub fn verify(&self, identity: Pubkey) -> bool {
        self.signature
            .verify(identity.as_array(), &self.slot.to_le_bytes())
    }
}
