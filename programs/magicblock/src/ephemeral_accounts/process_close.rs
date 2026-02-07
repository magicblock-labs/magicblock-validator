//! Close ephemeral account instruction processor

use solana_account::WritableAccount;
use solana_instruction::error::InstructionError;
use solana_log_collector::ic_msg;
use solana_program_runtime::invoke_context::InvokeContext;
use solana_sdk_ids::system_program;
use solana_transaction_context::TransactionContext;

use super::validation::{validate_common, validate_existing_ephemeral};
use super::{get_ephemeral_data_len, rent_for, transfer_rent};

/// Closes an ephemeral account, refunding rent to the sponsor.
pub(crate) fn process_close_ephemeral_account(
    invoke_context: &InvokeContext,
    transaction_context: &TransactionContext,
) -> Result<(), InstructionError> {
    let caller_program_id = validate_common(transaction_context)?;
    let ephemeral =
        validate_existing_ephemeral(transaction_context, &caller_program_id)?;

    let data_len = get_ephemeral_data_len(ephemeral)?;
    let refund = rent_for(data_len)?;
    transfer_rent(transaction_context, -(refund as i64))?;

    // Reset account to empty state
    let mut acc = ephemeral.borrow_mut();
    acc.set_lamports(0);
    acc.set_owner(system_program::id());
    acc.resize(0, 0);

    ic_msg!(invoke_context, "Closed ephemeral, refunded: {}", refund);
    Ok(())
}
