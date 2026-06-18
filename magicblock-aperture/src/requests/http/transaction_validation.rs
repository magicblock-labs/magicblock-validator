use std::mem::size_of;

use solana_message::VersionedMessage;
use solana_pubkey::Pubkey;
use solana_transaction::versioned::VersionedTransaction;

use crate::{error::RpcError, RpcResult};

// Solana's builtin-program filters in compute-budget processing assume program
// indices fit within a packet-bounded pubkey table (1232 / 32 = 38).
const MAX_RUNTIME_PROGRAM_ID_INDEX_EXCLUSIVE: usize =
    1232 / size_of::<Pubkey>();
const MAX_TX_ACCOUNT_LOCKS: usize = 1024;

pub(super) fn validate_supported_transaction_shape(
    transaction: &VersionedTransaction,
) -> RpcResult<()> {
    // Reject transactions that would lock more accounts than the runtime
    // permits BEFORE any expensive work (signature verification, sanitization,
    // account ensure/clone, scheduler locking). The HTTP body is not bounded to
    // a 1232-byte packet, so without this an attacker can submit a transaction
    // with thousands of account keys and amplify per-account work.
    let num_account_keys = transaction.message.static_account_keys().len();
    if num_account_keys > MAX_TX_ACCOUNT_LOCKS {
        return Err(RpcError::transaction_verification(format!(
            "transaction locks too many accounts: {num_account_keys}; max is {MAX_TX_ACCOUNT_LOCKS}"
        )));
    }

    if let VersionedMessage::V0(message) = &transaction.message {
        if !message.address_table_lookups.is_empty() {
            return Err(RpcError::transaction_verification(
                "v0 transactions with address lookup tables are not supported",
            ));
        }
    }

    for instruction in transaction.message.instructions() {
        let program_id_index = usize::from(instruction.program_id_index);
        if program_id_index >= MAX_RUNTIME_PROGRAM_ID_INDEX_EXCLUSIVE {
            return Err(RpcError::transaction_verification(format!(
                "unsupported program id index {program_id_index}; max supported is {}",
                MAX_RUNTIME_PROGRAM_ID_INDEX_EXCLUSIVE - 1
            )));
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use magicblock_core::link::blocks::BlockHash;
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

    #[test]
    fn rejects_program_id_index_outside_runtime_limit() {
        let transaction = VersionedTransaction {
            signatures: vec![Signature::default()],
            message: VersionedMessage::Legacy(Message {
                header: MessageHeader {
                    num_required_signatures: 1,
                    num_readonly_signed_accounts: 0,
                    num_readonly_unsigned_accounts: 38,
                },
                account_keys: {
                    let mut keys = vec![SYSTEM_PROGRAM_ID];
                    keys.extend(std::iter::repeat_n(SYSTEM_PROGRAM_ID, 37));
                    keys.push(COMPUTE_BUDGET_ID);
                    keys
                },
                recent_blockhash: BlockHash::new_unique(),
                instructions: vec![CompiledInstruction {
                    program_id_index: 38,
                    accounts: vec![],
                    data: vec![],
                }],
            }),
        };

        let error =
            validate_supported_transaction_shape(&transaction).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("unsupported program id index 38"),
            "unexpected error: {error}"
        );
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
