use std::collections::HashSet;

use magicblock_magic_program_api::{
    args::{CancelTaskRequest, TaskRequest},
    tls::ExecutionTlsStash,
};
use solana_log_collector::ic_msg;
use solana_program_runtime::invoke_context::InvokeContext;
use solana_sdk::{instruction::InstructionError, pubkey::Pubkey};

use crate::utils::accounts::get_instruction_pubkey_with_idx;

pub(crate) fn process_cancel_task(
    signers: HashSet<Pubkey>,
    invoke_context: &mut InvokeContext,
    task_id: u64,
) -> Result<(), InstructionError> {
    const TASK_AUTHORITY_IDX: u16 = 0;

    let transaction_context = &invoke_context.transaction_context.clone();

    // Validate that the task authority is a signer
    let task_authority_pubkey = get_instruction_pubkey_with_idx(
        transaction_context,
        TASK_AUTHORITY_IDX,
    )?;
    if !signers.contains(task_authority_pubkey) {
        ic_msg!(
            invoke_context,
            "CancelTask ERR: task authority {} not in signers",
            task_authority_pubkey
        );
        return Err(InstructionError::MissingRequiredSignature);
    }

    // Create cancel request
    let cancel_request = CancelTaskRequest {
        task_id,
        authority: *task_authority_pubkey,
    };

    // Add cancel request to execution TLS stash
    ExecutionTlsStash::register_task(TaskRequest::Cancel(cancel_request));

    ic_msg!(
        invoke_context,
        "Successfully added cancel request for task {}",
        task_id
    );

    Ok(())
}

#[cfg(test)]
mod test {
    use magicblock_magic_program_api::instruction::MagicBlockInstruction;
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
        let transaction_accounts = vec![(
            payer.pubkey(),
            AccountSharedData::new(u64::MAX, 0, &system_program::id()),
        )];
        let expected_result = Ok(());

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

        let account_metas = vec![AccountMeta::new(payer.pubkey(), false)];
        let ix = Instruction::new_with_bincode(
            crate::id(),
            &MagicBlockInstruction::CancelTask { task_id },
            account_metas,
        );
        let transaction_accounts = vec![(
            payer.pubkey(),
            AccountSharedData::new(u64::MAX, 0, &system_program::id()),
        )];
        let expected_result = Err(InstructionError::MissingRequiredSignature);

        process_instruction(
            &ix.data,
            transaction_accounts,
            ix.accounts,
            expected_result,
        );
    }
}
