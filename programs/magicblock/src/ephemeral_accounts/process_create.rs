//! Create ephemeral account instruction processor

use solana_account::WritableAccount;
use solana_instruction::error::InstructionError;
use solana_log_collector::ic_msg;
use solana_program_runtime::invoke_context::InvokeContext;
use solana_transaction_context::TransactionContext;

use super::{
    rent_for, transfer_rent,
    validation::{
        validate_common, validate_ephemeral_signer, validate_new_ephemeral,
    },
    MAX_DATA_LEN,
};

/// Creates a new ephemeral account with rent paid by the sponsor.
/// The account is owned by the calling program (inferred from CPI context).
pub(crate) fn process_create_ephemeral_account(
    invoke_context: &InvokeContext,
    transaction_context: &TransactionContext,
    data_len: u32,
) -> Result<(), InstructionError> {
    if data_len > MAX_DATA_LEN {
        return Err(InstructionError::InvalidArgument);
    }

    let caller_program_id = validate_common(transaction_context)?;
    validate_ephemeral_signer(transaction_context)?;
    let ephemeral = validate_new_ephemeral(transaction_context)?;

    let rent = rent_for(data_len)?;
    transfer_rent(transaction_context, rent as i64)?;

    // Initialize ephemeral account
    let mut acc = ephemeral.borrow_mut();
    acc.set_lamports(0);
    acc.set_owner(caller_program_id);
    acc.resize(data_len as usize, 0);
    acc.set_ephemeral(true);

    ic_msg!(
        invoke_context,
        "Created ephemeral: {} bytes, {} rent, owner: {}",
        data_len,
        rent,
        caller_program_id
    );
    Ok(())
}
