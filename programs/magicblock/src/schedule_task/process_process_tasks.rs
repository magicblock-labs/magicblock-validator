use std::collections::HashSet;

use solana_log_collector::ic_msg;
use solana_program_runtime::invoke_context::InvokeContext;
use solana_sdk::{instruction::InstructionError, pubkey::Pubkey};

use crate::{
    schedule_task::utils::check_task_context_id,
    task_context::TaskContext,
    utils::accounts::{
        get_instruction_account_with_idx, get_instruction_pubkey_with_idx,
    },
    validator::validator_authority_id,
};

pub(crate) fn process_process_tasks(
    signers: HashSet<Pubkey>,
    invoke_context: &mut InvokeContext,
) -> Result<(), InstructionError> {
    const PROCESSOR_AUTHORITY_IDX: u16 = 0;
    const TASK_CONTEXT_IDX: u16 = PROCESSOR_AUTHORITY_IDX + 1;

    check_task_context_id(invoke_context, TASK_CONTEXT_IDX)?;

    let transaction_context = &invoke_context.transaction_context.clone();

    // Validate that the task authority is a signer
    let processor_authority_pubkey = get_instruction_pubkey_with_idx(
        transaction_context,
        PROCESSOR_AUTHORITY_IDX,
    )?;
    if !signers.contains(processor_authority_pubkey) {
        ic_msg!(
            invoke_context,
            "ProcessTasks ERR: processor authority {} not in signers",
            processor_authority_pubkey
        );
        return Err(InstructionError::MissingRequiredSignature);
    }

    // Validate that the processor authority is the validator authority
    if processor_authority_pubkey.ne(&validator_authority_id()) {
        ic_msg!(
            invoke_context,
            "ProcessTasks ERR: processor authority {} is not the validator authority",
            processor_authority_pubkey
        );
        return Err(InstructionError::MissingRequiredSignature);
    }

    // Get the task context account
    let context_acc = get_instruction_account_with_idx(
        transaction_context,
        TASK_CONTEXT_IDX,
    )?;
    TaskContext::clear_requests(context_acc)?;

    ic_msg!(
        invoke_context,
        "Successfully cleared requests from task context",
    );

    Ok(())
}

#[cfg(test)]
mod test {
    use magicblock_magic_program_api::{
        instruction::MagicBlockInstruction, TASK_CONTEXT_PUBKEY,
    };
    use solana_sdk::{
        account::AccountSharedData,
        instruction::{AccountMeta, Instruction, InstructionError},
        signature::Keypair,
        signer::Signer,
        system_program,
    };

    use crate::{
        instruction_utils::InstructionUtils,
        test_utils::process_instruction,
        validator::{
            generate_validator_authority_if_needed, validator_authority_id,
        },
        TaskContext,
    };

    #[test]
    fn test_process_tasks() {
        generate_validator_authority_if_needed();

        let ix = InstructionUtils::process_tasks_instruction(
            &validator_authority_id(),
        );
        let transaction_accounts = vec![
            (
                validator_authority_id(),
                AccountSharedData::new(u64::MAX, 0, &system_program::id()),
            ),
            (
                TASK_CONTEXT_PUBKEY,
                AccountSharedData::new(
                    u64::MAX,
                    TaskContext::SIZE,
                    &system_program::id(),
                ),
            ),
        ];

        process_instruction(
            &ix.data,
            transaction_accounts,
            ix.accounts,
            Ok(()),
        );
    }

    #[test]
    fn fail_process_tasks_wrong_context() {
        generate_validator_authority_if_needed();

        let ix = InstructionUtils::process_tasks_instruction(
            &validator_authority_id(),
        );
        let wrong_context = Keypair::new().pubkey();
        let transaction_accounts = vec![
            (
                validator_authority_id(),
                AccountSharedData::new(u64::MAX, 0, &system_program::id()),
            ),
            (
                wrong_context,
                AccountSharedData::new(
                    u64::MAX,
                    TaskContext::SIZE,
                    &system_program::id(),
                ),
            ),
        ];

        process_instruction(
            &ix.data,
            transaction_accounts,
            ix.accounts,
            Err(InstructionError::MissingAccount),
        );
    }

    #[test]
    fn fail_process_tasks_wrong_authority() {
        generate_validator_authority_if_needed();

        let wrong_authority = Keypair::new().pubkey();
        let ix = InstructionUtils::process_tasks_instruction(&wrong_authority);
        let transaction_accounts = vec![
            (
                wrong_authority,
                AccountSharedData::new(u64::MAX, 0, &system_program::id()),
            ),
            (
                TASK_CONTEXT_PUBKEY,
                AccountSharedData::new(
                    u64::MAX,
                    TaskContext::SIZE,
                    &system_program::id(),
                ),
            ),
        ];

        process_instruction(
            &ix.data,
            transaction_accounts,
            ix.accounts,
            Err(InstructionError::MissingRequiredSignature),
        );
    }

    #[test]
    fn fail_unsigned_process_tasks() {
        generate_validator_authority_if_needed();

        let account_metas = vec![
            AccountMeta::new(validator_authority_id(), false),
            AccountMeta::new(TASK_CONTEXT_PUBKEY, false),
        ];
        let ix = Instruction::new_with_bincode(
            crate::id(),
            &MagicBlockInstruction::ProcessTasks,
            account_metas,
        );

        let transaction_accounts = vec![
            (
                validator_authority_id(),
                AccountSharedData::new(u64::MAX, 0, &system_program::id()),
            ),
            (
                TASK_CONTEXT_PUBKEY,
                AccountSharedData::new(
                    u64::MAX,
                    TaskContext::SIZE,
                    &system_program::id(),
                ),
            ),
        ];

        process_instruction(
            &ix.data,
            transaction_accounts,
            ix.accounts,
            Err(InstructionError::MissingRequiredSignature),
        );
    }
}
