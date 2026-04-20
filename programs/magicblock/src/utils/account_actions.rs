use solana_account::{ReadableAccount, WritableAccount};
use solana_instruction::error::InstructionError;

use super::DELEGATION_PROGRAM_ID;
use crate::utils::accounts::InstructionAccount;

/// Sets proper account values during undelegation
pub(crate) fn mark_account_as_undelegated(
    acc: &InstructionAccount<'_, '_>,
) -> Result<(), InstructionError> {
    let mut acc = acc.borrow_mut()?;
    acc.set_owner(DELEGATION_PROGRAM_ID);
    acc.set_undelegating(true);
    acc.set_delegated(false);
    Ok(())
}

/// Transfers `fee` lamports from `payer` to `fee_vault`.
/// Both accounts must be delegated; writability is enforced by the SVM.
pub(crate) fn charge_delegated_payer(
    payer: &InstructionAccount<'_, '_>,
    fee_vault: &InstructionAccount<'_, '_>,
    fee: u64,
) -> Result<(), InstructionError> {
    if !payer.borrow()?.delegated() {
        return Err(InstructionError::IllegalOwner);
    }
    if !fee_vault.borrow()?.delegated() {
        return Err(InstructionError::IllegalOwner);
    }
    if fee == 0 {
        return Ok(());
    }

    let new_payer_lamports = payer
        .borrow()?
        .lamports()
        .checked_sub(fee)
        .ok_or(InstructionError::InsufficientFunds)?;
    payer.borrow_mut()?.set_lamports(new_payer_lamports);

    let new_vault_lamports = fee_vault
        .borrow()?
        .lamports()
        .checked_add(fee)
        .ok_or(InstructionError::ArithmeticOverflow)?;
    fee_vault.borrow_mut()?.set_lamports(new_vault_lamports);

    Ok(())
}
