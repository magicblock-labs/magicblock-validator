use magicblock_magic_program_api::POST_DELEGATION_ACTION_EXECUTOR_PROGRAM_ID;
use solana_instruction::Instruction;
use solana_log_collector::ic_msg;
use solana_program_runtime::invoke_context::InvokeContext;
use solana_transaction::InstructionError;

use crate::{
    clone_account::load_instructions_sysvar_data,
    errors::MagicBlockProgramError, utils::instruction_sysvar,
};

pub fn validate_current_top_level_instruction(
    invoke_context: &mut InvokeContext,
    sysvar_data: &mut [u8],
    current_index: usize,
    context: &str,
) -> Result<(), InstructionError> {
    if invoke_context
        .transaction_context
        .get_instruction_stack_height()
        != 1
    {
        ic_msg!(invoke_context, "{} must be top-level", context);
        return Err(InstructionError::InvalidInstructionData);
    }

    let current_instruction =
        instruction_sysvar::load_instruction_at(sysvar_data, current_index)?;
    let current_ix_ctx = invoke_context
        .transaction_context
        .get_current_instruction_context()?;
    if current_instruction.program_id
        != POST_DELEGATION_ACTION_EXECUTOR_PROGRAM_ID
        || current_instruction.data != current_ix_ctx.get_instruction_data()
    {
        ic_msg!(invoke_context, "{} must be top-level", context);
        return Err(InstructionError::InvalidInstructionData);
    }
    Ok(())
}

pub fn validate_last_after_clone(
    invoke_context: &mut InvokeContext,
    context: &str,
) -> Result<Instruction, InstructionError> {
    let mut sysvar_data = load_instructions_sysvar_data(invoke_context)?;
    let current_index =
        instruction_sysvar::load_current_index(&mut sysvar_data)?;
    validate_current_top_level_instruction(
        invoke_context,
        &mut sysvar_data,
        current_index,
        context,
    )?;
    if current_index == 0 {
        ic_msg!(
            invoke_context,
            "{} has no previous clone instruction",
            context
        );
        return Err(
            MagicBlockProgramError::PostDelegationActionExecutorMissing.into(),
        );
    }

    // The executor mutates process-global pending-clone state for final chunked
    // clones, so it must be the last top-level instruction in the transaction.
    if instruction_sysvar::load_instruction_at(
        &mut sysvar_data,
        current_index.saturating_add(1),
    )
    .is_ok()
    {
        ic_msg!(invoke_context, "{} must be the last instruction", context);
        return Err(
            MagicBlockProgramError::PostDelegationActionExecutorMismatch.into(),
        );
    }

    let previous_instruction = instruction_sysvar::load_instruction_at(
        &mut sysvar_data,
        current_index - 1,
    )?;
    if previous_instruction.program_id != crate::ID {
        ic_msg!(
            invoke_context,
            "{} previous instruction is not Magic",
            context
        );
        return Err(
            MagicBlockProgramError::PostDelegationActionExecutorMismatch.into(),
        );
    }

    Ok(previous_instruction)
}
