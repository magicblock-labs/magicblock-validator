use solana_packet::PACKET_DATA_SIZE;
use wincode::{SchemaWrite, config::DefaultConfig};

/// Maximum serialized transaction size that can be sent over the wire.
pub(crate) const MAX_TRANSACTION_WIRE_SIZE: usize = PACKET_DATA_SIZE;

pub fn serialized_transaction_size<T>(transaction: &T) -> usize
where
    T: SchemaWrite<DefaultConfig, Src = T> + ?Sized,
{
    wincode::serialized_size(transaction)
        .map(|size| size as usize)
        .unwrap_or(usize::MAX)
}
