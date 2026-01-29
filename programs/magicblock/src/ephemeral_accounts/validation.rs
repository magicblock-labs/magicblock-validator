//! Validation utilities for ephemeral account instructions

use std::cell::RefCell;

use magicblock_magic_program_api::EPHEMERAL_VAULT_PUBKEY;
use solana_account::{AccountSharedData, ReadableAccount};
use solana_instruction::error::InstructionError;
use solana_pubkey::Pubkey;
use solana_transaction_context::TransactionContext;

use crate::utils::{
    accounts, instruction_context_frames::InstructionContextFrames,
};

/// Returns the caller program ID (the program that invoked us via CPI).
pub(crate) fn get_caller_program_id(
    transaction_context: &TransactionContext,
) -> Result<Pubkey, InstructionError> {
    let frames = InstructionContextFrames::try_from(transaction_context)?;
    frames
        .find_program_id_of_parent_of_current_instruction()
        .copied()
        .ok_or(InstructionError::IncorrectProgramId)
}

/// Validates that the instruction is called via CPI (not directly from a transaction).
pub(crate) fn validate_cpi_only(
    transaction_context: &TransactionContext,
) -> Result<(), InstructionError> {
    let frames = InstructionContextFrames::try_from(transaction_context)?;
    if frames
        .find_program_id_of_parent_of_current_instruction()
        .is_none()
    {
        return Err(InstructionError::IncorrectProgramId);
    }
    Ok(())
}

/// Validates the sponsor account (index 0):
/// - Oncurve accounts must be a signer
/// - PDA accounts must be owned by the calling program
pub(crate) fn validate_sponsor(
    transaction_context: &TransactionContext,
) -> Result<(), InstructionError> {
    let ix_ctx = transaction_context.get_current_instruction_context()?;

    if !ix_ctx.is_instruction_account_signer(0)? {
        // Not a signer - must be a PDA owned by the calling program
        let caller_program_id = get_caller_program_id(transaction_context)?;
        let sponsor_owner = accounts::get_instruction_account_owner_with_idx(
            transaction_context,
            0,
        )?;
        if sponsor_owner != caller_program_id {
            return Err(InstructionError::InvalidAccountOwner);
        }
    }
    Ok(())
}

/// Validates the vault account (index 2) matches the expected pubkey.
pub(crate) fn validate_vault(
    transaction_context: &TransactionContext,
) -> Result<(), InstructionError> {
    let vault_pubkey =
        accounts::get_instruction_pubkey_with_idx(transaction_context, 2)?;
    if *vault_pubkey != EPHEMERAL_VAULT_PUBKEY {
        return Err(InstructionError::InvalidAccountOwner);
    }
    Ok(())
}

/// Validates an existing ephemeral account (index 1):
/// - Must be marked as ephemeral
/// - Must be owned by the caller program
///
/// Returns the account for further operations.
pub(crate) fn validate_existing_ephemeral<'a>(
    transaction_context: &'a TransactionContext,
    caller_program_id: &Pubkey,
) -> Result<&'a RefCell<AccountSharedData>, InstructionError> {
    let ephemeral =
        accounts::get_instruction_account_with_idx(transaction_context, 1)?;

    let acc = ephemeral.borrow();
    if !acc.ephemeral() {
        return Err(InstructionError::InvalidAccountData);
    }
    if acc.owner() != caller_program_id {
        return Err(InstructionError::InvalidAccountOwner);
    }
    drop(acc);

    Ok(ephemeral)
}
