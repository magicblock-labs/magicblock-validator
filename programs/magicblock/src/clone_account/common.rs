//! Shared utilities for clone account instruction processing.

use std::{cell::RefCell, collections::HashSet};

use solana_account::{AccountSharedData, ReadableAccount, WritableAccount};
use solana_instruction::error::InstructionError;
use solana_log_collector::ic_msg;
use solana_program_runtime::invoke_context::InvokeContext;
use solana_pubkey::Pubkey;
use solana_transaction_context::TransactionContext;

use crate::validator::validator_authority_id;

/// Validates that the validator authority has signed the transaction.
/// All clone instructions require this signature for authorization.
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

/// Adjusts validator authority lamports to maintain balanced transactions.
///
/// # Lamports Flow
///
/// When cloning an account, we set the target account's lamports to the required rent-exempt
/// amount. The difference (delta) between new and old lamports is debited/credited from the
/// validator authority account:
///
/// - `delta > 0`: Account gained lamports → debit from authority
/// - `delta < 0`: Account lost lamports → credit to authority
///
/// This ensures the runtime doesn't complain about unbalanced transactions.
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
