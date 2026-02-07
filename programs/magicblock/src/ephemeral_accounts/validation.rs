//! Validation helpers for ephemeral account instructions.

use std::cell::RefCell;

use magicblock_magic_program_api::EPHEMERAL_VAULT_PUBKEY;
use solana_account::{AccountSharedData, ReadableAccount};
use solana_instruction::error::InstructionError;
use solana_pubkey::Pubkey;
use solana_sdk_ids::system_program;
use solana_transaction_context::TransactionContext;

use super::{EPHEMERAL_IDX, SPONSOR_IDX, VAULT_IDX};
use crate::utils::{
    accounts, instruction_context_frames::InstructionContextFrames,
};

/// Returns the program ID of the CPI caller.
///
/// Fails with [`InstructionError::IncorrectProgramId`] when invoked
/// outside of CPI, so this implicitly rejects direct top-level calls.
fn get_caller_program_id(
    tc: &TransactionContext,
) -> Result<Pubkey, InstructionError> {
    let frames = InstructionContextFrames::try_from(tc)?;
    frames
        .find_program_id_of_parent_of_current_instruction()
        .copied()
        .ok_or(InstructionError::IncorrectProgramId)
}

/// Validates that the sponsor account is a signer.
/// PDAs may satisfy this via `invoke_signed`.
fn validate_sponsor(
    tc: &TransactionContext,
) -> Result<(), InstructionError> {
    let ix_ctx = tc.get_current_instruction_context()?;
    if !ix_ctx.is_instruction_account_signer(SPONSOR_IDX)? {
        return Err(InstructionError::MissingRequiredSignature);
    }
    Ok(())
}

/// Validates the vault account matches the expected pubkey.
fn validate_vault(
    tc: &TransactionContext,
) -> Result<(), InstructionError> {
    let vault_pubkey =
        accounts::get_instruction_pubkey_with_idx(tc, VAULT_IDX)?;
    if *vault_pubkey != EPHEMERAL_VAULT_PUBKEY {
        return Err(InstructionError::InvalidAccountOwner);
    }
    Ok(())
}

// ----- Public helpers consumed by the three processors -----

/// Common validation sequence shared by all ephemeral account instructions.
///
/// Checks CPI context, sponsor signature, and vault identity.
/// Returns the caller program ID on success.
pub(super) fn validate_common(
    tc: &TransactionContext,
) -> Result<Pubkey, InstructionError> {
    let caller_program_id = get_caller_program_id(tc)?;
    validate_sponsor(tc)?;
    validate_vault(tc)?;
    Ok(caller_program_id)
}

/// Validates that the ephemeral account is a signer (prevents pubkey squatting).
/// Only required for account creation.
pub(super) fn validate_ephemeral_signer(
    tc: &TransactionContext,
) -> Result<(), InstructionError> {
    let ix_ctx = tc.get_current_instruction_context()?;
    if !ix_ctx.is_instruction_account_signer(EPHEMERAL_IDX)? {
        return Err(InstructionError::MissingRequiredSignature);
    }
    Ok(())
}

/// Validates that the account at [`EPHEMERAL_IDX`] is an empty system-owned
/// account (0 lamports, system program owner). Returns the account for
/// initialization.
pub(super) fn validate_new_ephemeral(
    tc: &TransactionContext,
) -> Result<&RefCell<AccountSharedData>, InstructionError> {
    let ephemeral =
        accounts::get_instruction_account_with_idx(tc, EPHEMERAL_IDX)?;
    let acc = ephemeral.borrow();
    if acc.lamports() != 0 || *acc.owner() != system_program::ID {
        return Err(InstructionError::InvalidAccountData);
    }
    drop(acc);
    Ok(ephemeral)
}

/// Validates an existing ephemeral account is marked ephemeral and owned by
/// the caller program. Returns the account for further operations.
pub(super) fn validate_existing_ephemeral<'a>(
    tc: &'a TransactionContext,
    caller_program_id: &Pubkey,
) -> Result<&'a RefCell<AccountSharedData>, InstructionError> {
    let ephemeral =
        accounts::get_instruction_account_with_idx(tc, EPHEMERAL_IDX)?;

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
