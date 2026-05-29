use base64::{prelude::BASE64_STANDARD, Engine};
use solana_rpc_client::rpc_client::SerializableTransaction;

/// From agave rpc/src/rpc.rs [MAX_BASE64_SIZE]
pub(crate) const MAX_ENCODED_TRANSACTION_SIZE: usize = 1644;

pub fn serialize_and_encode_base64(
    transaction: &impl SerializableTransaction,
) -> String {
    let serialized = bincode::serialize(transaction).unwrap();
    BASE64_STANDARD.encode(serialized)
}
