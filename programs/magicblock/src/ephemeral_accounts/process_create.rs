//! Create ephemeral account instruction processor

use magicblock_magic_program_api::id;
use solana_account::{ReadableAccount, WritableAccount};
use solana_instruction::error::InstructionError;
use solana_log_collector::ic_msg;
use solana_program_runtime::invoke_context::InvokeContext;
use solana_transaction_context::TransactionContext;

use super::processor::rent_for;
use super::validation::{validate_cpi_only, validate_sponsor};
use crate::utils::accounts;

/// Creates a new ephemeral account with rent paid by the sponsor.
/// The account is owned by the calling program (inferred from CPI context).
pub(crate) fn process_create_ephemeral_account(
    invoke_context: &InvokeContext,
    transaction_context: &TransactionContext,
    data_len: usize,
) -> Result<(), InstructionError> {
    use crate::utils::instruction_context_frames::InstructionContextFrames;

    // Must be called via CPI (user programs mediate all access)
    validate_cpi_only(transaction_context)?;

    // Get caller program ID (will be the owner of the ephemeral account)
    let frames = InstructionContextFrames::try_from(transaction_context)?;
    let caller_program_id = frames
        .find_program_id_of_parent_of_current_instruction()
        .ok_or(InstructionError::IncorrectProgramId)?;

    // Validate sponsor (signer or PDA owned by caller)
    validate_sponsor(transaction_context)?;

    // Validate vault is owned by magic program
    let vault =
        accounts::get_instruction_account_with_idx(transaction_context, 2)?;
    if *vault.borrow().owner() != id() {
        return Err(InstructionError::InvalidAccountOwner);
    }

    // Validate: must have 0 lamports and not already ephemeral
    let ephemeral =
        accounts::get_instruction_account_with_idx(transaction_context, 1)?;
    let acc = ephemeral.borrow();
    if acc.lamports() != 0 || acc.ephemeral() {
        return Err(InstructionError::InvalidAccountData);
    }
    drop(acc);

    // Debit rent from sponsor, credit to vault
    let rent = rent_for(data_len);
    accounts::debit_instruction_account_at_index(transaction_context, 0, rent)?;
    accounts::credit_instruction_account_at_index(
        transaction_context,
        2,
        rent,
    )?;

    // Set up ephemeral account (owned by the calling program)
    let mut acc = ephemeral.borrow_mut();
    acc.set_lamports(0);
    acc.set_owner(*caller_program_id);
    acc.resize(data_len, 0);
    acc.set_ephemeral(true);
    acc.set_delegated(true);

    ic_msg!(
        invoke_context,
        "Created ephemeral: {} bytes, {} rent, owner: {}",
        data_len,
        rent,
        caller_program_id
    );
    Ok(())
}
