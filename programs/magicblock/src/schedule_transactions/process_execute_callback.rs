use std::collections::HashSet;

use magicblock_magic_program_api::{pda::CALLBACK_SIGNER, CALLBACK_PROGRAM_ID};
use solana_instruction::{error::InstructionError, Instruction};
use solana_log_collector::ic_msg;
use solana_program_runtime::invoke_context::InvokeContext;

use crate::{
    schedule_transactions::validate_callback_accounts,
    utils::accounts::get_instruction_pubkey_with_idx,
    validator::validator_authority_id, Pubkey,
};

const VALIDATOR_IDX: u16 = 0;
const CALLBACK_SIGNER_IDX: u16 = 1;

/// Propagates callback defined by user
pub(crate) fn process_execute_callback(
    signers: HashSet<Pubkey>,
    invoke_context: &mut InvokeContext,
    instruction: Instruction,
) -> Result<(), InstructionError> {
    ic_msg!(invoke_context, "ExecuteCallback ERR: processing");
    validate(signers, invoke_context)?;
    ic_msg!(invoke_context, "ExecuteCallback ERR: validated");
    validate_callback_accounts(&invoke_context, &instruction.accounts)?;

    invoke_context.native_invoke(instruction, &[CALLBACK_SIGNER])
}

/// Checks if callback is correctly authorized
/// 1. Signed by validator
/// 2. Contains necessary accounts
/// 3. Account presence required by inner instruction are checked by `invokeContext::native_invoke`
fn validate(
    signers: HashSet<Pubkey>,
    invoke_context: &mut InvokeContext,
) -> Result<(), InstructionError> {
    let transaction_context = &invoke_context.transaction_context;
    let ix_ctx = transaction_context.get_current_instruction_context()?;

    // Assert callback executor program.
    let program_key = ix_ctx.get_program_key()?;
    if program_key != &CALLBACK_PROGRAM_ID {
        ic_msg!(
            invoke_context,
            "ExecuteCallback ERR: callback executor program account not found"
        );
        return Err(InstructionError::UnsupportedProgramId);
    }

    // Assert Validator is signer
    // Only the validator can execute a callback
    let validator_pubkey =
        get_instruction_pubkey_with_idx(transaction_context, VALIDATOR_IDX)?;
    if validator_pubkey != &validator_authority_id() {
        ic_msg!(
            invoke_context,
            "ExecuteCallback ERR: validator pubkey {} is not the expected validator",
            validator_pubkey
        );
        return Err(InstructionError::IncorrectAuthority);
    }
    if !signers.contains(validator_pubkey) {
        ic_msg!(
            invoke_context,
            "ExecuteCallback ERR: validator pubkey {} is not in signers",
            validator_pubkey
        );
        return Err(InstructionError::MissingRequiredSignature);
    }

    // Assert Callback signer is provided
    let callback_signer_pubkey = get_instruction_pubkey_with_idx(
        transaction_context,
        CALLBACK_SIGNER_IDX,
    )?;
    if callback_signer_pubkey != &CALLBACK_SIGNER {
        ic_msg!(
            invoke_context,
            "ExecuteCallback ERR: callback signer pubkey {} is not the expected Callback signer",
            callback_signer_pubkey
        );
        return Err(InstructionError::InvalidSeeds);
    }

    Ok(())
}
