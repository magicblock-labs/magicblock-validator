//! Close ephemeral account instruction processor

use magicblock_magic_program_api::id;
use solana_account::{ReadableAccount, WritableAccount};
use solana_instruction::error::InstructionError;
use solana_log_collector::ic_msg;
use solana_program_runtime::invoke_context::InvokeContext;
use solana_transaction_context::TransactionContext;

use super::processor::rent_for;
use super::validation::{validate_cpi_only, validate_sponsor};
use crate::utils::accounts;

/// Closes an ephemeral account, refunding rent to the sponsor.
pub(crate) fn process_close_ephemeral_account(
    invoke_context: &InvokeContext,
    transaction_context: &TransactionContext,
) -> Result<(), InstructionError> {
    // Must be called via CPI (user programs mediate all access)
    validate_cpi_only(transaction_context)?;

    // Validate sponsor (signer or PDA owned by caller)
    validate_sponsor(transaction_context)?;

    // Validate vault is owned by magic program
    let vault = accounts::get_instruction_account_with_idx(transaction_context, 2)?;
    if *vault.borrow().owner() != id() {
        return Err(InstructionError::InvalidAccountOwner);
    }

    let ephemeral = accounts::get_instruction_account_with_idx(transaction_context, 1)?;

    if !ephemeral.borrow().ephemeral() {
        return Err(InstructionError::InvalidAccountData);
    }

    let data_len = ephemeral.borrow().data().len();
    let refund = rent_for(data_len);
    // Credit sponsor, debit vault
    accounts::credit_instruction_account_at_index(
        transaction_context,
        0,
        refund,
    )?;
    accounts::debit_instruction_account_at_index(transaction_context, 2, refund)?;

    let mut acc = ephemeral.borrow_mut();
    acc.set_lamports(0);
    acc.set_owner(solana_sdk_ids::system_program::id());
    acc.resize(0, 0);
    acc.set_ephemeral(false);
    acc.set_delegated(false);

    ic_msg!(invoke_context, "Closed ephemeral, refunded: {}", refund);
    Ok(())
}
