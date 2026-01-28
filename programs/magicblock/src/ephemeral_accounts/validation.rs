//! Validation utilities for ephemeral account instructions

use solana_instruction::error::InstructionError;
use solana_transaction_context::TransactionContext;

/// Validates the sponsor account according to MIMD-0016 spec:
/// - Oncurve accounts: must be a signer
/// - PDA accounts: must be owned by the calling program
pub(crate) fn validate_sponsor(
    transaction_context: &TransactionContext,
) -> Result<(), InstructionError> {
    use crate::utils::{
        accounts, instruction_context_frames::InstructionContextFrames,
    };

    let ix_ctx = transaction_context.get_current_instruction_context()?;

    // Check if sponsor is a signer
    if !ix_ctx.is_instruction_account_signer(0)? {
        // Not a signer, must be a PDA owned by the calling program
        let frames = InstructionContextFrames::try_from(transaction_context)?;
        let caller_program_id = frames
            .find_program_id_of_parent_of_current_instruction()
            .ok_or(InstructionError::InvalidAccountData)?;

        let sponsor_owner = accounts::get_instruction_account_owner_with_idx(
            transaction_context,
            0,
        )?;
        if sponsor_owner != *caller_program_id {
            return Err(InstructionError::InvalidAccountOwner);
        }
    }
    Ok(())
}

/// Validates that the instruction is being called via CPI (not directly from a transaction).
/// This ensures all ephemeral account operations are mediated by user programs,
/// which implement their own access control and business logic.
pub(crate) fn validate_cpi_only(
    transaction_context: &TransactionContext,
) -> Result<(), InstructionError> {
    use crate::utils::instruction_context_frames::InstructionContextFrames;

    let frames = InstructionContextFrames::try_from(transaction_context)?;
    let caller_program_id =
        frames.find_program_id_of_parent_of_current_instruction();

    // If caller_program_id is None, we're at the top level (direct call, not CPI)
    if caller_program_id.is_none() {
        return Err(InstructionError::IncorrectProgramId);
    }

    Ok(())
}
