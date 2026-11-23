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
/// Current base-layer account-lock limit observed on Devnet. This counts all
/// resolved transaction accounts, including read-only and ALT-loaded keys.
pub(crate) const MAX_BASE_LAYER_TX_ACCOUNT_LOCKS: usize = 64;

/// Counts all account keys a compiled transaction resolves and locks.
/// Address lookup tables compress keys on the wire, but their loaded addresses
/// still count as transaction account locks.
pub(crate) fn resolved_account_lock_count(message: &VersionedMessage) -> usize {
    match message {
        VersionedMessage::Legacy(message) => message.account_keys.len(),
        VersionedMessage::V0(message) => {
            message.account_keys.len()
                + message
                    .address_table_lookups
                    .iter()
                    .map(|lookup| {
                        lookup.writable_indexes.len()
                            + lookup.readonly_indexes.len()
                    })
                    .sum::<usize>()
        }
    }
}

pub fn serialized_transaction_size(
    transaction: &impl SerializableTransaction,
) -> usize {
    // SAFETY: runs on transactions we already serialize before sending.
    usize::try_from(bincode::serialized_size(transaction).unwrap()).unwrap()
}

#[cfg(test)]
mod tests {
    use solana_message::{v0::MessageAddressTableLookup, VersionedMessage};
    use solana_pubkey::Pubkey;

    use super::{resolved_account_lock_count, MAX_BASE_LAYER_TX_ACCOUNT_LOCKS};

    #[test]
    fn resolved_account_lock_count_includes_alt_loaded_keys() {
        let lookup = MessageAddressTableLookup {
            account_key: Pubkey::new_unique(),
            writable_indexes: (0..32).collect(),
            readonly_indexes: (32..63).collect(),
        };
        let mut message = solana_message::v0::Message {
            account_keys: vec![Pubkey::new_unique()],
            address_table_lookups: vec![lookup],
            ..Default::default()
        };
        let versioned_message = VersionedMessage::V0(message.clone());
        assert_eq!(
            resolved_account_lock_count(&versioned_message),
            MAX_BASE_LAYER_TX_ACCOUNT_LOCKS
        );

        message.address_table_lookups[0].readonly_indexes.push(63);
        let versioned_message = VersionedMessage::V0(message);
        assert_eq!(
            resolved_account_lock_count(&versioned_message),
            MAX_BASE_LAYER_TX_ACCOUNT_LOCKS + 1
        );
    }
}
