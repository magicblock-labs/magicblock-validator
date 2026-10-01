use solana_hash::Hash;
use solana_message::VersionedMessage;
use solana_packet::PACKET_DATA_SIZE;
use solana_rpc_client::rpc_client::SerializableTransaction;

pub mod v1;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PreparedMessage {
    Versioned(VersionedMessage),
    V1(v1::Message),
}

impl PreparedMessage {
    pub fn set_recent_blockhash(&mut self, recent_blockhash: Hash) {
        match self {
            Self::Versioned(message) => {
                message.set_recent_blockhash(recent_blockhash);
            }
            Self::V1(message) => {
                message.set_recent_blockhash(recent_blockhash);
            }
        }
    }
}

impl PartialEq<VersionedMessage> for PreparedMessage {
    fn eq(&self, other: &VersionedMessage) -> bool {
        match (self, other) {
            (Self::Versioned(message), other) => message == other,
            (Self::V1(message), VersionedMessage::V0(other)) => {
                message.matches_v0_message(other)
            }
            _ => false,
        }
    }
}

/// Maximum serialized v0 transaction size that can be sent over the wire.
pub(crate) const MAX_TRANSACTION_WIRE_SIZE: usize = PACKET_DATA_SIZE;
/// Maximum serialized v1 transaction size that can be sent over the wire.
pub(crate) const MAX_TRANSACTION_V1_WIRE_SIZE: usize = 4096;

pub fn serialized_transaction_size(
    transaction: &impl SerializableTransaction,
) -> usize {
    // SAFETY: runs on transactions we already serialize before sending.
    usize::try_from(bincode::serialized_size(transaction).unwrap()).unwrap()
}
