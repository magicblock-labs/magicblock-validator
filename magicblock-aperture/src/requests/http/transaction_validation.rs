use solana_message::VersionedMessage;
use solana_transaction::versioned::VersionedTransaction;

use crate::{error::RpcError, RpcResult};

pub(super) fn validate_supported_transaction_shape(
    transaction: &VersionedTransaction,
) -> RpcResult<()> {
    if let VersionedMessage::V0(message) = &transaction.message {
        if !message.address_table_lookups.is_empty() {
            return Err(RpcError::transaction_verification(
                "v0 transactions with address lookup tables are not supported",
            ));
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use magicblock_core::link::{
        blocks::BlockHash, transactions::SanitizeableTransaction,
    };
    use solana_message::{
        compiled_instruction::CompiledInstruction,
        legacy::Message,
        v0::{Message as V0Message, MessageAddressTableLookup},
        MessageHeader, VersionedMessage,
    };
    use solana_pubkey::Pubkey;
    use solana_signature::Signature;
    use solana_transaction::versioned::VersionedTransaction;

    use super::validate_supported_transaction_shape;

    const SYSTEM_PROGRAM_ID: Pubkey =
        Pubkey::from_str_const("11111111111111111111111111111111");
    const COMPUTE_BUDGET_ID: Pubkey =
        Pubkey::from_str_const("ComputeBudget111111111111111111111111111111");

    #[test]
    fn accepts_program_id_index_within_runtime_limit() {
        let transaction = VersionedTransaction {
            signatures: vec![Signature::default()],
            message: VersionedMessage::Legacy(Message {
                header: MessageHeader {
                    num_required_signatures: 1,
                    num_readonly_signed_accounts: 0,
                    num_readonly_unsigned_accounts: 37,
                },
                account_keys: {
                    let mut keys = vec![SYSTEM_PROGRAM_ID];
                    keys.extend(std::iter::repeat_n(SYSTEM_PROGRAM_ID, 36));
                    keys.push(COMPUTE_BUDGET_ID);
                    keys
                },
                recent_blockhash: BlockHash::new_unique(),
                instructions: vec![CompiledInstruction {
                    program_id_index: 37,
                    accounts: vec![],
                    data: vec![],
                }],
            }),
        };

        validate_supported_transaction_shape(&transaction).unwrap();
    }

    /// ER messages may use the full native program-index range without a packet limit.
    #[test]
    fn accepts_program_id_index_beyond_packet_limit() {
        let transaction = VersionedTransaction {
            signatures: vec![Signature::default()],
            message: VersionedMessage::Legacy(Message {
                header: MessageHeader {
                    num_required_signatures: 1,
                    num_readonly_signed_accounts: 0,
                    num_readonly_unsigned_accounts: 255,
                },
                account_keys: {
                    let mut keys = vec![SYSTEM_PROGRAM_ID];
                    keys.extend((0..254).map(|_| Pubkey::new_unique()));
                    keys.push(COMPUTE_BUDGET_ID);
                    keys
                },
                recent_blockhash: BlockHash::new_unique(),
                instructions: vec![CompiledInstruction {
                    program_id_index: 255,
                    accounts: vec![],
                    data: vec![],
                }],
            }),
        };

        validate_supported_transaction_shape(&transaction).unwrap();
        transaction.sanitize(false).unwrap();
    }

    #[test]
    fn rejects_v0_transactions_with_address_lookup_tables() {
        let transaction = VersionedTransaction {
            signatures: vec![Signature::default()],
            message: VersionedMessage::V0(V0Message {
                header: MessageHeader {
                    num_required_signatures: 1,
                    num_readonly_signed_accounts: 0,
                    num_readonly_unsigned_accounts: 1,
                },
                account_keys: vec![SYSTEM_PROGRAM_ID],
                recent_blockhash: BlockHash::new_unique(),
                instructions: vec![],
                address_table_lookups: vec![MessageAddressTableLookup {
                    account_key: Pubkey::new_unique(),
                    writable_indexes: vec![0],
                    readonly_indexes: vec![1],
                }],
            }),
        };

        let error =
            validate_supported_transaction_shape(&transaction).unwrap_err();
        assert!(
            error.to_string().contains(
                "v0 transactions with address lookup tables are not supported"
            ),
            "unexpected error: {error}"
        );
    }
}
