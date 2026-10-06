use solana_message::v1;
use solana_packet::PACKET_DATA_SIZE;
use solana_transaction::versioned::VersionedTransaction;

/// Maximum serialized legacy/v0 transaction size.
pub(crate) const MAX_TRANSACTION_WIRE_SIZE: usize = PACKET_DATA_SIZE;
/// Maximum serialized native v1 transaction size.
pub(crate) const MAX_TRANSACTION_V1_WIRE_SIZE: usize = v1::MAX_TRANSACTION_SIZE;

pub fn serialized_transaction_size(
    transaction: &VersionedTransaction,
) -> usize {
    // Constructed transactions have already passed native message validation.
    wincode::serialized_size(transaction).unwrap() as usize
}
