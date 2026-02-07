//! Resize ephemeral account instruction processor

use solana_instruction::error::InstructionError;
use solana_log_collector::ic_msg;
use solana_program_runtime::invoke_context::InvokeContext;
use solana_transaction_context::TransactionContext;

use super::validation::{validate_common, validate_existing_ephemeral};
use super::{get_ephemeral_data_len, rent_for, transfer_rent, MAX_DATA_LEN};

/// Resizes an existing ephemeral account, adjusting rent accordingly.
pub(crate) fn process_resize_ephemeral_account(
    invoke_context: &InvokeContext,
    transaction_context: &TransactionContext,
    new_data_len: u32,
) -> Result<(), InstructionError> {
    if new_data_len > MAX_DATA_LEN {
        return Err(InstructionError::InvalidArgument);
    }

    let caller_program_id = validate_common(transaction_context)?;
    let ephemeral =
        validate_existing_ephemeral(transaction_context, &caller_program_id)?;

    let old_len = get_ephemeral_data_len(ephemeral)?;
    let new_rent = rent_for(new_data_len)?;
    let old_rent = rent_for(old_len)?;
    let delta = new_rent as i64 - old_rent as i64;

    transfer_rent(transaction_context, delta)?;

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
