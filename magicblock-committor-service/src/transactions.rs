use solana_packet::PACKET_DATA_SIZE;
use wincode::{SchemaWrite, config::DefaultConfig};

/// Maximum serialized transaction size that can be sent over the wire.
pub(crate) const MAX_TRANSACTION_WIRE_SIZE: usize = PACKET_DATA_SIZE;

pub fn serialized_transaction_size<T>(transaction: &T) -> usize
where
    T: SchemaWrite<DefaultConfig, Src = T> + ?Sized,
{
    // SAFETY: runs on transactions we already serialize before sending.
    usize::try_from(wincode::serialized_size(transaction).unwrap()).unwrap()
}
