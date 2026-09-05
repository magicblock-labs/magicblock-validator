use solana_packet::PACKET_DATA_SIZE;
use thiserror::Error;
use wincode::{SchemaWrite, config::DefaultConfig, error::WriteError};

/// Maximum serialized transaction size that can be sent over the wire.
pub(crate) const MAX_TRANSACTION_WIRE_SIZE: usize = PACKET_DATA_SIZE;

#[derive(Debug, Error)]
pub enum SerializedTransactionSizeError {
    #[error("Failed to compute serialized transaction size: {0}")]
    Serialize(#[from] WriteError),
    #[error("Serialized transaction size does not fit in usize")]
    SizeOverflow,
}

pub fn serialized_transaction_size<T>(
    transaction: &T,
) -> Result<usize, SerializedTransactionSizeError>
where
    T: SchemaWrite<DefaultConfig, Src = T> + ?Sized,
{
    let size = wincode::serialized_size(transaction)?;
    usize::try_from(size)
        .map_err(|_| SerializedTransactionSizeError::SizeOverflow)
}
