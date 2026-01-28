//! Resize ephemeral account instruction processor

use magicblock_magic_program_api::id;
use solana_account::ReadableAccount;
use solana_instruction::error::InstructionError;
use solana_log_collector::ic_msg;
use solana_program_runtime::invoke_context::InvokeContext;
use solana_transaction_context::TransactionContext;

use super::{
    processor::rent_for,
    validation::{validate_cpi_only, validate_sponsor},
};
use crate::utils::accounts;

/// Resizes an existing ephemeral account, adjusting rent accordingly.
pub(crate) fn process_resize_ephemeral_account(
    invoke_context: &InvokeContext,
    transaction_context: &TransactionContext,
    new_data_len: usize,
) -> Result<(), InstructionError> {
    // Must be called via CPI (user programs mediate all access)
    validate_cpi_only(transaction_context)?;

    // Validate sponsor (signer or PDA owned by caller)
    validate_sponsor(transaction_context)?;

    // Validate vault is owned by magic program
    let vault =
        accounts::get_instruction_account_with_idx(transaction_context, 2)?;
    if *vault.borrow().owner() != id() {
        return Err(InstructionError::InvalidAccountOwner);
    }

    let ephemeral =
        accounts::get_instruction_account_with_idx(transaction_context, 1)?;

    if !ephemeral.borrow().ephemeral() {
        return Err(InstructionError::InvalidAccountData);
    }

    let old_len = ephemeral.borrow().data().len();
    let delta = rent_for(new_data_len) as i64 - rent_for(old_len) as i64;

    if delta > 0 {
        // Debit sponsor, credit vault
        accounts::debit_instruction_account_at_index(
            transaction_context,
            0,
            delta as u64,
        )?;
        accounts::credit_instruction_account_at_index(
            transaction_context,
            2,
            delta as u64,
        )?;
    } else {
        // Credit sponsor, debit vault
        accounts::credit_instruction_account_at_index(
            transaction_context,
            0,
            delta.unsigned_abs(),
        )?;
        accounts::debit_instruction_account_at_index(
            transaction_context,
            2,
            delta.unsigned_abs(),
        )?;
    }

    ephemeral.borrow_mut().resize(new_data_len, 0);

    ic_msg!(
        invoke_context,
        "Resized: {} -> {} bytes, delta: {}",
        old_len,
        new_data_len,
        delta
    );
    Ok(())
}
