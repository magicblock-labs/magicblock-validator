#![allow(dead_code)]
use std::{cmp::Ordering, mem::size_of, sync::Arc};

use sleipnir_bank::get_compute_budget_details::{
    ComputeBudgetDetails, GetComputeBudgetDetails,
};
use solana_sdk::{
    feature_set,
    hash::Hash,
    message::Message,
    packet::Packet,
    sanitize::SanitizeError,
    short_vec::decode_shortu16_len,
    signature::Signature,
    transaction::{
        AddressLoader, SanitizedTransaction, SanitizedVersionedTransaction,
        VersionedTransaction,
    },
};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum DeserializedPacketError {
    #[error("ShortVec Failed to Deserialize")]
    // short_vec::decode_shortu16_len() currently returns () on error
    ShortVecError(()),
    #[error("Deserialization Error: {0}")]
    DeserializationError(#[from] bincode::Error),
    #[error("overflowed on signature size {0}")]
    SignatureOverflowed(usize),
    #[error("packet failed sanitization {0}")]
    SanitizeError(#[from] SanitizeError),
    #[error("transaction failed prioritization")]
    PrioritizationFailure,
    #[error("vote transaction failure")]
    VoteTransactionError,
}

// NOTE: removed
// - original_packet
//  - even if we use a Packet for sending transacions we don't need it anymore since it is only
//  needed for forward actions which we don't support
// - is simple vote
#[derive(Debug, PartialEq, Eq)]
pub struct ImmutableDeserializedPacket {
    transaction: SanitizedVersionedTransaction,
    message_hash: Hash,
    compute_budget_details: ComputeBudgetDetails,
}

impl ImmutableDeserializedPacket {
    pub fn new(packet: Packet) -> Result<Self, DeserializedPacketError> {
        let versioned_transaction: VersionedTransaction =
            packet.deserialize_slice(..)?;
        let message_bytes = packet_message(&packet)?;
        let message_hash = Message::hash_raw_message(message_bytes);

        Self::new_from_versioned_transaction(
            versioned_transaction,
            Some(message_hash),
        )
    }

    pub fn new_from_versioned_transaction(
        versioned_transaction: VersionedTransaction,
        hash: Option<Hash>,
    ) -> Result<Self, DeserializedPacketError> {
        let sanitized_transaction =
            SanitizedVersionedTransaction::try_from(versioned_transaction)?;

        let round_compute_unit_price = true;
        let compute_budget_details = sanitized_transaction
            .get_compute_budget_details(round_compute_unit_price)
            .ok_or(DeserializedPacketError::PrioritizationFailure)?;

        // NOTE: removed vote transaction case

        let message_hash = hash.unwrap_or_else(|| {
            sanitized_transaction.get_message().message.hash()
        });

        Ok(Self {
            transaction: sanitized_transaction,
            message_hash,

            compute_budget_details,
        })
    }

    pub fn transaction(&self) -> &SanitizedVersionedTransaction {
        &self.transaction
    }

    pub fn message_hash(&self) -> &Hash {
        &self.message_hash
    }

    pub fn is_simple_vote(&self) -> bool {
        false
    }

    pub fn compute_unit_price(&self) -> u64 {
        self.compute_budget_details.compute_unit_price
    }

    pub fn compute_unit_limit(&self) -> u64 {
        self.compute_budget_details.compute_unit_limit
    }

    pub fn compute_budget_details(&self) -> ComputeBudgetDetails {
        self.compute_budget_details.clone()
    }

    // This function deserializes packets into transactions, computes the blake3 hash of transaction
    // messages, and verifies secp256k1 instructions.
    pub fn build_sanitized_transaction(
        &self,
        feature_set: &Arc<feature_set::FeatureSet>,
        address_loader: impl AddressLoader,
    ) -> Option<SanitizedTransaction> {
        // NOTE: we don't support vote transactions
        let is_simple_vote = false;
        let tx = SanitizedTransaction::try_new(
            self.transaction().clone(),
            *self.message_hash(),
            is_simple_vote,
            address_loader,
        )
        .ok()?;
        tx.verify_precompiles(feature_set).ok()?;
        Some(tx)
    }
}

impl PartialOrd for ImmutableDeserializedPacket {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for ImmutableDeserializedPacket {
    fn cmp(&self, other: &Self) -> Ordering {
        self.compute_unit_price().cmp(&other.compute_unit_price())
    }
}

/// Read the transaction message from packet data
fn packet_message(packet: &Packet) -> Result<&[u8], DeserializedPacketError> {
    let (sig_len, sig_size) = packet
        .data(..)
        .and_then(|bytes| decode_shortu16_len(bytes).ok())
        .ok_or(DeserializedPacketError::ShortVecError(()))?;
    sig_len
        .checked_mul(size_of::<Signature>())
        .and_then(|v| v.checked_add(sig_size))
        .and_then(|msg_start| packet.data(msg_start..))
        .ok_or(DeserializedPacketError::SignatureOverflowed(sig_size))
}
