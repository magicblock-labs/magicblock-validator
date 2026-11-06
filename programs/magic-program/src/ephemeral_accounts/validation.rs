//! Validation helpers for ephemeral account instructions.

use magic_program_api::EPHEMERAL_VAULT_PUBKEY;
use solana_account::{AccountMode, ReadableAccount, StateFlags};
use solana_instruction::error::InstructionError;
use solana_program_runtime::invoke_context::InvokeContext;
use solana_pubkey::Pubkey;
use solana_sdk_ids::system_program;
use solana_transaction_context::transaction::TransactionContext;

use super::{EPHEMERAL_IDX, SPONSOR_IDX, VAULT_IDX};
use crate::utils::accounts::{self, InstructionAccount};

/// Common validation sequence shared by all ephemeral account instructions.
///
/// Checks CPI context, sponsor signature, and vault identity.
/// Returns the caller program ID on success.
pub(super) fn validate_common(
    invoke_context: &InvokeContext<'_, '_>,
) -> Result<Pubkey, InstructionError> {
    let tc = &*invoke_context.transaction_context;
    let caller = invoke_context.effective_caller()?.ok_or(InstructionError::IncorrectProgramId)?;
    let ix_ctx = tc.get_current_instruction_context()?;
    if !ix_ctx.is_instruction_account_signer(SPONSOR_IDX)? {
        return Err(InstructionError::MissingRequiredSignature);
    }
    let vault = accounts::get_instruction_pubkey_with_idx(tc, VAULT_IDX)?;
    if *vault != EPHEMERAL_VAULT_PUBKEY {
        return Err(InstructionError::InvalidAccountData);
    }
    Ok(caller)
}

/// Requires the target's signature to prevent pubkey squatting and zero-balance,
/// system-owned state ready for initialization.
pub(super) fn validate_new_ephemeral<'a, 'ix_data>(
    tc: &'a TransactionContext<'ix_data>,
) -> Result<InstructionAccount<'a, 'ix_data>, InstructionError> {
    let ix_ctx = tc.get_current_instruction_context()?;
    if !ix_ctx.is_instruction_account_signer(EPHEMERAL_IDX)? {
        return Err(InstructionError::MissingRequiredSignature);
    }
    let ephemeral = accounts::get_instruction_account_with_idx(tc, EPHEMERAL_IDX)?;
    let acc = ephemeral.borrow()?;
    if acc.lamports() != 0 || *acc.owner() != system_program::ID {
        return Err(InstructionError::InvalidAccountData);
    }
    drop(acc);
    Ok(ephemeral)
}

/// Requires rent-funded Magic state owned by the caller; local writability
/// alone does not entitle an account to refunds from the shared vault.
pub(super) fn validate_existing_ephemeral<'a, 'ix_data>(
    tc: &'a TransactionContext<'ix_data>,
    caller_program_id: &Pubkey,
) -> Result<InstructionAccount<'a, 'ix_data>, InstructionError> {
    let ephemeral = accounts::get_instruction_account_with_idx(tc, EPHEMERAL_IDX)?;
    let ep_ref = ephemeral.borrow()?;

    if !ep_ref.is(AccountMode::Magic) || !ep_ref.flags().contains(StateFlags::RENT_FUNDED) {
        return Err(InstructionError::InvalidAccountData);
    }
    if ep_ref.owner() != caller_program_id {
        return Err(InstructionError::InvalidAccountOwner);
    }
    drop(ep_ref);

    Ok(ephemeral)
}
