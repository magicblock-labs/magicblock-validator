//! Create Magic account instruction processor

use magic_program_api::magic_account;
use solana_account::{AccountMode, StateFlags, WritableAccount};
use solana_instruction::error::InstructionError;
use solana_log_collector::ic_msg;
use solana_program_runtime::invoke_context::InvokeContext;
use solana_transaction_context::transaction::TransactionContext;

use super::{
    MAX_MAGIC_ACCOUNT_DATA_LEN, transfer_rent,
    validation::{validate_common, validate_new_magic_account},
};
use crate::utils::account_actions::set_account_mode;

/// Creates a new Magic account with rent paid by the sponsor.
/// The account is owned by the calling program (inferred from CPI context).
pub(crate) fn process_create_magic_account(
    invoke_context: &InvokeContext<'_, '_>,
    transaction_context: &TransactionContext<'_>,
    data_len: u32,
) -> Result<(), InstructionError> {
    if data_len > MAX_MAGIC_ACCOUNT_DATA_LEN {
        return Err(InstructionError::InvalidArgument);
    }

    let caller_program_id = validate_common(invoke_context)?;
    let magic_account = validate_new_magic_account(transaction_context)?;

    let rent = magic_account::rent_for(data_len)?;
    transfer_rent(transaction_context, rent as i64)?;

    let mut acc = magic_account.borrow_mut()?;
    acc.set_lamports(0);
    acc.set_owner(caller_program_id);
    acc.resize(data_len as usize, 0);
    set_account_mode(invoke_context, &mut acc, AccountMode::Magic)?;
    let flags = *acc.flags() | StateFlags::RENT_FUNDED;
    acc.set_flags(flags);

    ic_msg!(
        invoke_context,
        "Created Magic account: {} bytes, {} rent, owner: {}",
        data_len,
        rent,
        caller_program_id
    );
    Ok(())
}
