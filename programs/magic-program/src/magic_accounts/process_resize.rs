//! Resize Magic account instruction processor

use magic_program_api::magic_account;
use solana_instruction::error::InstructionError;
use solana_log_collector::ic_msg;
use solana_program_runtime::invoke_context::InvokeContext;
use solana_transaction_context::transaction::TransactionContext;

use super::{
    MAX_MAGIC_ACCOUNT_DATA_LEN, get_magic_account_data_len, transfer_rent,
    validation::{validate_common, validate_existing_magic_account},
};

/// Resizes an existing Magic account, adjusting rent accordingly.
pub(crate) fn process_resize_magic_account(
    invoke_context: &InvokeContext<'_, '_>,
    transaction_context: &TransactionContext<'_>,
    new_data_len: u32,
) -> Result<(), InstructionError> {
    if new_data_len > MAX_MAGIC_ACCOUNT_DATA_LEN {
        return Err(InstructionError::InvalidArgument);
    }

    let caller_program_id = validate_common(invoke_context)?;
    let magic_account = validate_existing_magic_account(transaction_context, &caller_program_id)?;

    let old_len = get_magic_account_data_len(&magic_account)?;
    let new_rent = magic_account::rent_for(new_data_len)?;
    let old_rent = magic_account::rent_for(old_len)?;
    let delta = new_rent as i64 - old_rent as i64;

    transfer_rent(transaction_context, delta)?;

    magic_account.borrow_mut()?.resize(new_data_len as usize, 0);

    ic_msg!(
        invoke_context,
        "Resized: {} -> {} bytes, delta: {}",
        old_len,
        new_data_len,
        delta
    );
    Ok(())
}
