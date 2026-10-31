// https://solana.com/docs/core/transactions#transaction-size

use committor_interface::{
    consts::MAX_INSTRUCTION_DATA_SIZE, instruction::IX_WRITE_SIZE_WITHOUT_CHUNKS,
};
pub(crate) use solana_message::v1::MAX_TRANSACTION_SIZE as MAX_TRANSACTION_V1_WIRE_SIZE;
use solana_packet::PACKET_DATA_SIZE;
use wincode::{SchemaWrite, config::DefaultConfig};

const BUDGET_SET_COMPUTE_UNIT_PRICE_BYTES: u16 = (1 + 8) * 8;
const BUDGET_SET_COMPUTE_UNIT_LIMIT_BYTES: u16 = (1 + 4) * 8;
/// The maximum size of a chunk that can be written as part of a single transaction
pub(crate) const MAX_WRITE_CHUNK_SIZE: u16 = MAX_INSTRUCTION_DATA_SIZE
    - IX_WRITE_SIZE_WITHOUT_CHUNKS
    - BUDGET_SET_COMPUTE_UNIT_PRICE_BYTES
    - BUDGET_SET_COMPUTE_UNIT_LIMIT_BYTES;
/// Maximum serialized v0 transaction size that can be sent over the wire.
pub(crate) const MAX_TRANSACTION_WIRE_SIZE: usize = PACKET_DATA_SIZE;

pub(crate) fn serialized_transaction_size<T>(transaction: &T) -> usize
where
    T: SchemaWrite<DefaultConfig, Src = T> + ?Sized,
{
    wincode::serialized_size(transaction)
        .map(|size| size as usize)
        .unwrap_or(usize::MAX)
}
