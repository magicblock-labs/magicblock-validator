use std::collections::HashSet;

use solana_log_collector::ic_msg;
use solana_program_runtime::invoke_context::InvokeContext;
use solana_sdk::{instruction::InstructionError, pubkey::Pubkey};

use crate::{
    schedule_task::utils::check_task_context_id,
    task_context::{CancelTaskRequest, TaskContext},
    utils::accounts::{
        get_instruction_account_with_idx, get_instruction_pubkey_with_idx,
    },
};

pub(crate) fn process_cancel_task(
    signers: HashSet<Pubkey>,
    invoke_context: &mut InvokeContext,
    task_id: u64,
) -> Result<(), InstructionError> {
    const TASK_AUTHORITY_IDX: u16 = 0;
    const TASK_CONTEXT_IDX: u16 = TASK_AUTHORITY_IDX + 1;

    check_task_context_id(invoke_context, TASK_CONTEXT_IDX)?;

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

    // Get the task context account
    let context_acc = get_instruction_account_with_idx(
        transaction_context,
        TASK_CONTEXT_IDX,
    )?;

    TaskContext::cancel_task(invoke_context, context_acc, cancel_request)
        .map_err(|err| {
            ic_msg!(
                invoke_context,
                "CancelTask ERR: failed to cancel task: {}",
                err
            );
            InstructionError::GenericError
        })?;

    ic_msg!(
        invoke_context,
        "Successfully added cancel request for task {}",
        task_id
    );

    Ok(())
}
