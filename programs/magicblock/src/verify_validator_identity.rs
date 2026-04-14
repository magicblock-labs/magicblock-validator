use std::collections::HashSet;

use solana_instruction::error::InstructionError;
use solana_log_collector::ic_msg;
use solana_program_runtime::invoke_context::InvokeContext;
use solana_pubkey::Pubkey;

use crate::{
    schedule_transactions, utils::accounts::get_instruction_pubkey_with_idx,
    validator::validator_authority_id,
};

pub(crate) fn process_verify_validator_identity(
    _signers: HashSet<Pubkey>,
    invoke_context: &mut InvokeContext,
) -> Result<(), InstructionError> {
    const MAGIC_CONTEXT_IDX: u16 = 0;
    const VALIDATOR_ACCOUNT_IDX: u16 = MAGIC_CONTEXT_IDX + 1;

    schedule_transactions::check_magic_context_id(
        invoke_context,
        MAGIC_CONTEXT_IDX,
    )?;

    let transaction_context = &invoke_context.transaction_context;
    let ix_ctx = transaction_context.get_current_instruction_context()?;

    // Assert MagicBlock program
    ix_ctx
        .find_index_of_program_account(transaction_context, &crate::id())
        .ok_or_else(|| {
            ic_msg!(
                invoke_context,
                "ScheduleCommit ERR: Magic program account not found"
            );
            InstructionError::UnsupportedProgramId
        })?;

    let validator_identity = get_instruction_pubkey_with_idx(
        transaction_context,
        VALIDATOR_ACCOUNT_IDX,
    )?;
    let expected_validator_identity = validator_authority_id();
    if validator_identity == &expected_validator_identity {
        Ok(())
    } else {
        Err(InstructionError::IncorrectAuthority)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use magicblock_magic_program_api::{
        instruction::MagicBlockInstruction, MAGIC_CONTEXT_PUBKEY,
    };
    use serial_test::serial;
    use solana_account::AccountSharedData;
    use solana_instruction::{
        error::InstructionError, AccountMeta, Instruction,
    };
    use solana_pubkey::Pubkey;
    use solana_sdk_ids::system_program;

    use crate::{
        magic_context::MagicContext,
        test_utils::{
            ensure_started_validator, process_instruction, AUTHORITY_BALANCE,
        },
        validator::validator_authority_id,
    };

    fn verify_validator_identity_instruction(validator: Pubkey) -> Instruction {
        let account_metas = vec![
            AccountMeta::new_readonly(MAGIC_CONTEXT_PUBKEY, false),
            AccountMeta::new_readonly(validator, false),
        ];
        Instruction::new_with_bincode(
            crate::id(),
            &MagicBlockInstruction::VerifyValidatorIdentity,
            account_metas,
        )
    }

    fn setup_transaction_accounts() -> Vec<(Pubkey, AccountSharedData)> {
        let mut account_data = HashMap::new();
        account_data.insert(
            MAGIC_CONTEXT_PUBKEY,
            AccountSharedData::new(u64::MAX, MagicContext::SIZE, &crate::id()),
        );
        ensure_started_validator(&mut account_data, None);
        let validator = validator_authority_id();
        vec![
            (
                MAGIC_CONTEXT_PUBKEY,
                account_data.remove(&MAGIC_CONTEXT_PUBKEY).unwrap(),
            ),
            (validator, account_data.remove(&validator).unwrap()),
        ]
    }

    #[test]
    #[serial]
    fn test_verify_validator_identity_success() {
        let validator = validator_authority_id();
        let ix = verify_validator_identity_instruction(validator);
        let transaction_accounts = setup_transaction_accounts();
        process_instruction(
            &ix.data,
            transaction_accounts,
            ix.accounts,
            Ok(()),
        );
    }

    #[test]
    #[serial]
    fn test_verify_validator_identity_wrong_validator() {
        let wrong_validator = Pubkey::new_unique();
        let ix = verify_validator_identity_instruction(wrong_validator);

        let mut account_data = HashMap::new();
        account_data.insert(
            MAGIC_CONTEXT_PUBKEY,
            AccountSharedData::new(u64::MAX, MagicContext::SIZE, &crate::id()),
        );
        ensure_started_validator(&mut account_data, None);

        let transaction_accounts = vec![
            (
                MAGIC_CONTEXT_PUBKEY,
                account_data.remove(&MAGIC_CONTEXT_PUBKEY).unwrap(),
            ),
            (
                wrong_validator,
                AccountSharedData::new(
                    AUTHORITY_BALANCE,
                    0,
                    &system_program::id(),
                ),
            ),
        ];

        process_instruction(
            &ix.data,
            transaction_accounts,
            ix.accounts,
            Err(InstructionError::IncorrectAuthority),
        );
    }

    #[test]
    #[serial]
    fn test_verify_validator_identity_wrong_magic_context() {
        let validator = validator_authority_id();
        let wrong_context = Pubkey::new_unique();

        let account_metas = vec![
            AccountMeta::new_readonly(wrong_context, false),
            AccountMeta::new_readonly(validator, false),
        ];
        let ix = Instruction::new_with_bincode(
            crate::id(),
            &MagicBlockInstruction::VerifyValidatorIdentity,
            account_metas,
        );

        let mut account_data = HashMap::new();
        ensure_started_validator(&mut account_data, None);

        let transaction_accounts = vec![
            (
                wrong_context,
                AccountSharedData::new(0, 0, &system_program::id()),
            ),
            (validator, account_data.remove(&validator).unwrap()),
        ];

        process_instruction(
            &ix.data,
            transaction_accounts,
            ix.accounts,
            Err(InstructionError::MissingAccount),
        );
    }
}
