//! Resize ephemeral account instruction processor

use solana_account::ReadableAccount;
use solana_instruction::error::InstructionError;
use solana_log_collector::ic_msg;
use solana_program_runtime::invoke_context::InvokeContext;
use solana_transaction_context::TransactionContext;

use super::{
    processor::{rent_for, MAX_DATA_LEN},
    validation::{
        get_caller_program_id, validate_cpi_only, validate_existing_ephemeral,
        validate_sponsor, validate_vault,
    },
};
use crate::utils::accounts;

/// Resizes an existing ephemeral account, adjusting rent accordingly.
pub(crate) fn process_resize_ephemeral_account(
    invoke_context: &InvokeContext,
    transaction_context: &TransactionContext,
    new_data_len: u32,
) -> Result<(), InstructionError> {
    if new_data_len > MAX_DATA_LEN {
        return Err(InstructionError::InvalidArgument);
    }

    validate_cpi_only(transaction_context)?;
    validate_sponsor(transaction_context)?;
    validate_vault(transaction_context)?;

    let caller_program_id = get_caller_program_id(transaction_context)?;
    let ephemeral =
        validate_existing_ephemeral(transaction_context, &caller_program_id)?;

    let old_len: u32 = ephemeral
        .borrow()
        .data()
        .len()
        .try_into()
        .map_err(|_| InstructionError::ArithmeticOverflow)?;

    let new_rent = rent_for(new_data_len)?;
    let old_rent = rent_for(old_len)?;
    let delta = new_rent as i64 - old_rent as i64;

    if delta > 0 {
        // Growing: debit sponsor, credit vault
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
        // Shrinking: credit sponsor, debit vault
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

    ephemeral.borrow_mut().resize(new_data_len as usize, 0);

    ic_msg!(
        invoke_context,
        "Resized: {} -> {} bytes, delta: {}",
        old_len,
        new_data_len,
        delta
    );
    Ok(())
}
