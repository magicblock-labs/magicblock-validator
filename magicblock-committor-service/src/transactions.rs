use solana_message::VersionedMessage;
use solana_packet::PACKET_DATA_SIZE;
use solana_rpc_client::rpc_client::SerializableTransaction;

pub mod v1;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PreparedMessage {
    Versioned(VersionedMessage),
    V1(v1::Message),
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
