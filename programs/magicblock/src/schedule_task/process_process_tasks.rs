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
        instruction_utils::InstructionUtils, test_utils::process_instruction,
    };

    #[test]
    fn test_process_cancel_task() {
        let payer = Keypair::new();
        let task_id = 1;

        let ix =
            InstructionUtils::cancel_task_instruction(&payer.pubkey(), task_id);
        let transaction_accounts = vec![
            (
                payer.pubkey(),
                AccountSharedData::new(u64::MAX, 0, &system_program::id()),
            ),
            (
                TASK_CONTEXT_PUBKEY,
                AccountSharedData::new(u64::MAX, 0, &system_program::id()),
            ),
        ];
        let expected_result = Ok(());

        process_instruction(
            &ix.data,
            transaction_accounts,
            ix.accounts,
            expected_result,
        );
    }

    #[test]
    fn fail_process_cancel_task_wrong_context() {
        let payer = Keypair::new();
        let wrong_context = Keypair::new().pubkey();
        let task_id = 1;

        let account_metas = vec![
            AccountMeta::new(payer.pubkey(), true),
            AccountMeta::new(wrong_context, false),
        ];
        let ix = Instruction::new_with_bincode(
            crate::id(),
            &MagicBlockInstruction::CancelTask { task_id },
            account_metas,
        );
        let transaction_accounts = vec![
            (
                payer.pubkey(),
                AccountSharedData::new(u64::MAX, 0, &system_program::id()),
            ),
            (
                wrong_context,
                AccountSharedData::new(u64::MAX, 0, &system_program::id()),
            ),
        ];
        let expected_result = Err(InstructionError::MissingAccount);

        process_instruction(
            &ix.data,
            transaction_accounts,
            ix.accounts,
            expected_result,
        );
    }

    #[test]
    fn fail_unsigned_process_cancel_task() {
        let payer = Keypair::new();
        let task_id = 1;

        let account_metas = vec![
            AccountMeta::new(payer.pubkey(), false),
            AccountMeta::new(TASK_CONTEXT_PUBKEY, false),
        ];
        let ix = Instruction::new_with_bincode(
            crate::id(),
            &MagicBlockInstruction::CancelTask { task_id },
            account_metas,
        );
        let transaction_accounts = vec![
            (
                payer.pubkey(),
                AccountSharedData::new(u64::MAX, 0, &system_program::id()),
            ),
            (
                TASK_CONTEXT_PUBKEY,
                AccountSharedData::new(u64::MAX, 0, &system_program::id()),
            ),
        ];
        let expected_result = Err(InstructionError::MissingRequiredSignature);

        process_instruction(
            &ix.data,
            transaction_accounts,
            ix.accounts,
            expected_result,
        );
    }
}
