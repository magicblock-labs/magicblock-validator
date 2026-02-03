//! Create ephemeral account instruction processor

use solana_account::{ReadableAccount, WritableAccount};
use solana_instruction::error::InstructionError;
use solana_log_collector::ic_msg;
use solana_program_runtime::invoke_context::InvokeContext;
use solana_sdk_ids::system_program;
use solana_transaction_context::TransactionContext;

use super::validation::{
    get_caller_program_id, validate_cpi_only, validate_sponsor, validate_vault,
};
use crate::{
    ephemeral_accounts::{rent_for, MAX_DATA_LEN},
    utils::accounts,
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

    validate_cpi_only(transaction_context)?;
    validate_sponsor(transaction_context)?;
    validate_vault(transaction_context)?;

    // Ephemeral account must be a signer (prevents pubkey squatting)
    let ix_ctx = transaction_context.get_current_instruction_context()?;
    if !ix_ctx.is_instruction_account_signer(1)? {
        return Err(InstructionError::MissingRequiredSignature);
    }

    let caller_program_id = get_caller_program_id(transaction_context)?;

    // Target account must be empty (0 lamports, owned by system program)
    let ephemeral =
        accounts::get_instruction_account_with_idx(transaction_context, 1)?;
    let acc = ephemeral.borrow();
    if acc.lamports() != 0 || *acc.owner() != system_program::ID {
        return Err(InstructionError::InvalidAccountData);
    }
    drop(acc);

    // Transfer rent from sponsor to vault
    let rent = rent_for(data_len)?;
    accounts::debit_instruction_account_at_index(transaction_context, 0, rent)?;
    accounts::credit_instruction_account_at_index(
        transaction_context,
        2,
        rent,
    )?;

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
