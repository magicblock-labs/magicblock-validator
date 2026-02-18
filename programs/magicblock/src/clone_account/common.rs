//! Shared utilities for clone account and mutate account instruction processing.

use std::{cell::RefCell, collections::HashSet};

use solana_account::{AccountSharedData, ReadableAccount, WritableAccount};
use solana_instruction::error::InstructionError;
use solana_log_collector::ic_msg;
use solana_program_runtime::invoke_context::InvokeContext;
use solana_pubkey::Pubkey;
use solana_transaction_context::TransactionContext;

use crate::errors::MagicBlockProgramError;
use crate::validator::validator_authority_id;

/// Validates that the validator authority has signed the transaction.
pub fn validate_authority(
    signers: &HashSet<Pubkey>,
    invoke_context: &InvokeContext,
) -> Result<(), InstructionError> {
    let auth = validator_authority_id();
    if signers.contains(&auth) {
        return Ok(());
    }
    ic_msg!(invoke_context, "Validator authority '{auth}' not in signers");
    Err(InstructionError::MissingRequiredSignature)
}

/// Validates that the account at `ix_index` matches `expected` pubkey.
/// Returns the transaction-level index on success.
pub fn validate_and_get_index(
    transaction_context: &TransactionContext,
    ix_index: u16,
    expected: &Pubkey,
    name: &str,
    invoke_context: &InvokeContext,
) -> Result<u16, InstructionError> {
    let ctx = transaction_context.get_current_instruction_context()?;
    let tx_idx = ctx.get_index_of_instruction_account_in_transaction(ix_index)?;
    let key = transaction_context.get_key_of_account_at_index(tx_idx)?;
    if *key == *expected {
        return Ok(tx_idx);
    }
    ic_msg!(invoke_context, "{}: key mismatch, expected {}, got {}", name, expected, key);
    Err(InstructionError::InvalidArgument)
}

/// Returns true if account is ephemeral (exists locally on ER only).
pub fn is_ephemeral(account: &RefCell<AccountSharedData>) -> bool {
    account.borrow().ephemeral()
}

/// Validates that a delegated account is undelegating (mutation allowed).
pub fn validate_not_delegated(
    account: &RefCell<AccountSharedData>,
    pubkey: &Pubkey,
    invoke_context: &InvokeContext,
) -> Result<(), InstructionError> {
    let (is_delegated, is_undelegating) = {
        let acc = account.borrow();
        (acc.delegated(), acc.undelegating())
    };
    if is_delegated && !is_undelegating {
        ic_msg!(invoke_context, "Account {} is delegated and not undelegating", pubkey);
        return Err(MagicBlockProgramError::AccountIsDelegated.into());
    }
    Ok(())
}

/// Validates that the account can be mutated (not ephemeral, not active delegated).
pub fn validate_mutable(
    account: &RefCell<AccountSharedData>,
    pubkey: &Pubkey,
    invoke_context: &InvokeContext,
) -> Result<(), InstructionError> {
    if is_ephemeral(account) {
        ic_msg!(invoke_context, "Account {} is ephemeral and cannot be mutated", pubkey);
        return Err(MagicBlockProgramError::AccountIsEphemeral.into());
    }
    validate_not_delegated(account, pubkey, invoke_context)
}

/// Validates that incoming remote_slot is not older than current.
/// Skips check if incoming_remote_slot is None.
pub fn validate_remote_slot(
    account: &RefCell<AccountSharedData>,
    pubkey: &Pubkey,
    incoming_remote_slot: Option<u64>,
    invoke_context: &InvokeContext,
) -> Result<(), InstructionError> {
    let Some(incoming) = incoming_remote_slot else {
        return Ok(());
    };
    let current = account.borrow().remote_slot();
    if incoming < current {
        ic_msg!(
            invoke_context,
            "Account {} incoming remote_slot {} is older than current {}; rejected",
            pubkey, incoming, current
        );
        return Err(MagicBlockProgramError::OutOfOrderUpdate.into());
    }
    Ok(())
}

/// Adjusts validator authority lamports by delta.
/// Positive delta = debit, negative delta = credit.
pub fn adjust_authority_lamports(
    auth_acc: &RefCell<AccountSharedData>,
    delta: i64,
) -> Result<(), InstructionError> {
    if delta == 0 {
        return Ok(());
    }
    let auth_lamports = auth_acc.borrow().lamports();
    let adjusted = if delta > 0 {
        auth_lamports
            .checked_sub(delta as u64)
            .ok_or(InstructionError::InsufficientFunds)?
    } else {
        auth_lamports
            .checked_add((-delta) as u64)
            .ok_or(InstructionError::ArithmeticOverflow)?
    };
    auth_acc.borrow_mut().set_lamports(adjusted);
    Ok(())
}
