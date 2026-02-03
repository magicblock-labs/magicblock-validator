//! Close ephemeral account instruction processor

use solana_account::{ReadableAccount, WritableAccount};
use solana_instruction::error::InstructionError;
use solana_log_collector::ic_msg;
use solana_program_runtime::invoke_context::InvokeContext;
use solana_sdk_ids::system_program;
use solana_transaction_context::TransactionContext;

use super::{
    rent_for,
    validation::{
        get_caller_program_id, validate_cpi_only, validate_existing_ephemeral,
        validate_sponsor, validate_vault,
    },
};
use crate::utils::accounts;

/// Closes an ephemeral account, refunding rent to the sponsor.
pub(crate) fn process_close_ephemeral_account(
    invoke_context: &InvokeContext,
    transaction_context: &TransactionContext,
) -> Result<(), InstructionError> {
    validate_cpi_only(transaction_context)?;
    validate_sponsor(transaction_context)?;
    validate_vault(transaction_context)?;

    let caller_program_id = get_caller_program_id(transaction_context)?;
    let ephemeral =
        validate_existing_ephemeral(transaction_context, &caller_program_id)?;

    let data_len: u32 = ephemeral
        .borrow()
        .data()
        .len()
        .try_into()
        .map_err(|_| InstructionError::ArithmeticOverflow)?;

    // Transfer rent from vault back to sponsor
    let refund = rent_for(data_len)?;
    accounts::credit_instruction_account_at_index(
        transaction_context,
        0,
        refund,
    )?;
    accounts::debit_instruction_account_at_index(
        transaction_context,
        2,
        refund,
    )?;

    // Reset account to empty state
    let mut acc = ephemeral.borrow_mut();
    acc.set_lamports(0);
    acc.set_owner(system_program::id());
    acc.resize(0, 0);

    ic_msg!(invoke_context, "Closed ephemeral, refunded: {}", refund);
    Ok(())
}
