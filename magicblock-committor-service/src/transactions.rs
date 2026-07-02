use solana_packet::PACKET_DATA_SIZE;
use solana_rpc_client::rpc_client::SerializableTransaction;

/// Maximum serialized transaction size that can be sent over the wire.
pub(crate) const MAX_TRANSACTION_WIRE_SIZE: usize = PACKET_DATA_SIZE;

pub fn serialized_transaction_size(
    transaction: &impl SerializableTransaction,
) -> usize {
    // SAFETY: runs on transactions we already serialize before sending.
    usize::try_from(bincode::serialized_size(transaction).unwrap()).unwrap()
}
