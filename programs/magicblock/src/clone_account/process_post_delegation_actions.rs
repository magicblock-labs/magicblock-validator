use std::collections::HashSet;

use magicblock_magic_program_api::{
    instruction::MagicBlockInstruction,
    POST_DELEGATION_ACTION_EXECUTOR_PROGRAM_ID,
};
use solana_instruction::{error::InstructionError, Instruction};
use solana_log_collector::ic_msg;
use solana_program_runtime::invoke_context::InvokeContext;
use solana_pubkey::Pubkey;

use super::{
    execute_post_delegation_actions, load_instructions_sysvar_data,
    remove_pending_clone, validate_and_get_index, validate_authority,
};
use crate::{errors::MagicBlockProgramError, utils::instruction_sysvar};

pub(crate) fn process_execute_post_delegation_actions(
    signers: HashSet<Pubkey>,
    invoke_context: &mut InvokeContext,
    pubkey: Pubkey,
    actions: Vec<Instruction>,
) -> Result<(), InstructionError> {
    validate_authority(&signers, invoke_context)?;
    validate_program_account(invoke_context)?;

    let mut sysvar_data = load_instructions_sysvar_data(invoke_context)?;
    let current_index =
        instruction_sysvar::load_current_index(&mut sysvar_data)?;
    validate_current_top_level_instruction(
        invoke_context,
        &mut sysvar_data,
        current_index,
    )?;
    if current_index == 0 {
        ic_msg!(
            invoke_context,
            "Post-delegation action executor has no previous clone instruction"
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
        ic_msg!(
            invoke_context,
            "Post-delegation action executor must be the last instruction"
        );
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
            "Post-delegation action executor previous instruction is not Magic"
        );
        return Err(
            MagicBlockProgramError::PostDelegationActionExecutorMismatch.into(),
        );
    }

    let previous_magic_instruction: MagicBlockInstruction =
        bincode::deserialize(&previous_instruction.data)
            .map_err(|_| InstructionError::InvalidInstructionData)?;
    let previous_was_final_continue = match previous_magic_instruction {
        MagicBlockInstruction::CloneAccount {
            pubkey: previous_pubkey,
            fields,
            actions: previous_actions,
            ..
        } if previous_pubkey == pubkey && previous_actions == actions => {
            if !fields.delegated {
                return Err(
                    MagicBlockProgramError::PostDelegationActionsRequireDelegatedClone
                        .into(),
                );
            }
            false
        }
        MagicBlockInstruction::CloneAccountContinue {
            pubkey: previous_pubkey,
            is_last: true,
            actions: previous_actions,
            ..
        } if previous_pubkey == pubkey && previous_actions == actions => true,
        _ => {
            ic_msg!(
                invoke_context,
                "Post-delegation action executor previous instruction mismatch"
            );
            return Err(
                MagicBlockProgramError::PostDelegationActionExecutorMismatch
                    .into(),
            );
        }
    };

    let delegated_target = {
        let transaction_context = &invoke_context.transaction_context;
        let tx_idx = validate_and_get_index(
            transaction_context,
            1,
            &pubkey,
            "ExecutePostDelegationActions",
            invoke_context,
        )?;
        transaction_context
            .accounts()
            .try_borrow(tx_idx)?
            .delegated()
    };

    execute_post_delegation_actions(
        invoke_context,
        &pubkey,
        delegated_target,
        actions,
    )?;

    if previous_was_final_continue {
        remove_pending_clone(&pubkey);
    }

    Ok(())
}

fn validate_program_account(
    invoke_context: &mut InvokeContext,
) -> Result<(), InstructionError> {
    let transaction_context = &invoke_context.transaction_context;
    let ix_ctx = transaction_context.get_current_instruction_context()?;
    if ix_ctx.get_program_key()? != &POST_DELEGATION_ACTION_EXECUTOR_PROGRAM_ID
    {
        ic_msg!(
            invoke_context,
            "Post-delegation action executor program account not found"
        );
        return Err(InstructionError::UnsupportedProgramId);
    }
    Ok(())
}

fn validate_current_top_level_instruction(
    invoke_context: &mut InvokeContext,
    sysvar_data: &mut [u8],
    current_index: usize,
) -> Result<(), InstructionError> {
    if invoke_context
        .transaction_context
        .get_instruction_stack_height()
        != 1
    {
        ic_msg!(
            invoke_context,
            "Post-delegation action executor must be top-level"
        );
        return Err(
            MagicBlockProgramError::PostDelegationActionExecutorNotTopLevel
                .into(),
        );
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
        ic_msg!(
            invoke_context,
            "Post-delegation action executor must be top-level"
        );
        return Err(
            MagicBlockProgramError::PostDelegationActionExecutorNotTopLevel
                .into(),
        );
    }
    Ok(())
}
