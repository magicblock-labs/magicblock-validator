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

use crate::error::Result;

pub type Slot = u64;
pub type TxIndex = u32;

pub const PROTOCOL_VERSION: u32 = 1;

/// Top-level replication message.
#[derive(Deserialize, Serialize, Clone, Debug)]
pub enum Message {
    HandshakeReq(HandshakeRequest),
    HandshakeResp(HandshakeResponse),
    Transaction(Transaction),
    Block(Block),
    SuperBlock(SuperBlock),
    Failover(FailoverSignal),
}

/// Client -> Server: initiate replication session.
/// Authenticated via Ed25519 signature over `start_slot`.
#[derive(Deserialize, Serialize, Clone, Debug)]
pub struct HandshakeRequest {
    pub version: u32,
    pub start_slot: Slot,
    pub identity: Pubkey,
    signature: Signature,
}

/// Server -> Client: accept or reject session.
/// Signed over `slot` (success) or error message (failure).
#[derive(Deserialize, Serialize, Clone, Debug)]
pub struct HandshakeResponse {
    pub result: std::result::Result<Slot, String>,
    pub identity: Pubkey,
    signature: Signature,
}

/// Slot boundary marker with blockhash.
#[derive(Deserialize, Serialize, Clone, Debug)]
pub struct Block {
    pub slot: Slot,
    pub hash: Hash,
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

impl HandshakeRequest {
    pub fn new(start_slot: Slot, keypair: &Keypair) -> Self {
        Self {
            version: PROTOCOL_VERSION,
            start_slot,
            identity: keypair.pubkey(),
            signature: keypair.sign_message(&start_slot.to_le_bytes()),
        }
    }

    /// Verifies signature matches claimed identity.
    pub fn verify(&self) -> bool {
        self.signature
            .verify(self.identity.as_array(), &self.start_slot.to_le_bytes())
    }
}

impl HandshakeResponse {
    pub fn new(result: Result<Slot>, keypair: &Keypair) -> Self {
        let result = result.map_err(|e| e.to_string());
        let signature = match &result {
            Ok(slot) => keypair.sign_message(&slot.to_le_bytes()),
            Err(err) => keypair.sign_message(err.as_bytes()),
        };
        Self {
            result,
            identity: keypair.pubkey(),
            signature,
        }
    }

    /// Verifies signature matches server identity.
    pub fn verify(&self) -> bool {
        match &self.result {
            Ok(slot) => self
                .signature
                .verify(self.identity.as_array(), &slot.to_le_bytes()),
            Err(err) => self
                .signature
                .verify(self.identity.as_array(), err.as_bytes()),
        }
    }
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
