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

    // Get the task context account
    let context_acc = get_instruction_account_with_idx(
        transaction_context,
        TASK_CONTEXT_IDX,
    )?;
    let context_data = &mut context_acc.borrow_mut();
    let mut task_context =
        TaskContext::deserialize(context_data).map_err(|err| {
            ic_msg!(
                invoke_context,
                "Failed to deserialize TaskContext: {}",
                err
            );
            InstructionError::GenericError
        })?;

    // Remove the task
    let task = task_context
        .remove_task(task_id)
        .ok_or(InstructionError::InvalidArgument)?;

    // Validate that the signer is the task authority
    if task.authority != *task_authority_pubkey {
        ic_msg!(
            invoke_context,
            "Task authority mismatch: expected {}, got {}",
            task.authority,
            task_authority_pubkey
        );
        return Err(InstructionError::InvalidArgument);
    }

    // Update the account data
    context_data.serialize_data(&task_context).map_err(|err| {
        ic_msg!(invoke_context, "Failed to serialize TaskContext: {}", err);
        InstructionError::GenericError
    })?;

    ic_msg!(invoke_context, "Successfully cancelled task {}", task_id);

    Ok(())
}
