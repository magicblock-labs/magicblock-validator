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
    execute_post_delegation_actions, remove_pending_clone,
    validate_and_get_index, validate_authority,
};
use crate::{
    errors::MagicBlockProgramError,
    utils::validation::validate_last_after_clone,
};

pub(crate) fn process_execute_post_delegation_actions(
    signers: HashSet<Pubkey>,
    invoke_context: &mut InvokeContext,
    cloned_account_pubkey: Pubkey,
    actions: Vec<Instruction>,
) -> Result<(), InstructionError> {
    validate_authority(&signers, invoke_context)?;
    validate_program_account(invoke_context)?;
    let previous_instruction = validate_last_after_clone(
        invoke_context,
        "Post-delegation action executor",
    )?;

    let previous_magic_instruction: MagicBlockInstruction =
        bincode::deserialize(&previous_instruction.data)
            .map_err(|_| InstructionError::InvalidInstructionData)?;
    let previous_was_final_continue = match previous_magic_instruction {
        MagicBlockInstruction::CloneAccount {
            pubkey: previous_pubkey,
            fields,
            actions: previous_actions,
            ..
        } if previous_pubkey == cloned_account_pubkey
            && previous_actions == actions =>
        {
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
        } if previous_pubkey == cloned_account_pubkey
            && previous_actions == actions =>
        {
            true
        }
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
            &cloned_account_pubkey,
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
        &cloned_account_pubkey,
        delegated_target,
        actions,
    )?;

    if previous_was_final_continue {
        remove_pending_clone(&cloned_account_pubkey);
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
