use std::cell::RefCell;

use solana_account::{AccountSharedData, ReadableAccount, WritableAccount};
use solana_instruction::error::InstructionError;
use solana_pubkey::Pubkey;

use super::DELEGATION_PROGRAM_ID;

pub(crate) fn set_account_owner(
    acc: &RefCell<AccountSharedData>,
    pubkey: Pubkey,
) {
    acc.borrow_mut().set_owner(pubkey);
}

/// Sets proper account values during undelegation
pub(crate) fn mark_account_as_undelegated(acc: &RefCell<AccountSharedData>) {
    set_account_owner(acc, DELEGATION_PROGRAM_ID);
    let mut acc = acc.borrow_mut();
    acc.set_undelegating(true);
    acc.set_delegated(false);
}

/// Transfers `fee` lamports from `payer` to `fee_vault`.
/// Both accounts must be delegated; writability is enforced by the SVM.
pub(crate) fn charge_delegated_payer(
    payer: &RefCell<AccountSharedData>,
    fee_vault: &RefCell<AccountSharedData>,
    fee: u64,
) -> Result<(), InstructionError> {
    if !payer.borrow().delegated() {
        return Err(InstructionError::IllegalOwner);
    }
    if !fee_vault.borrow().delegated() {
        return Err(InstructionError::IllegalOwner);
    }
    if fee == 0 {
        return Ok(());
    }

    let new_payer_lamports = payer
        .borrow()
        .lamports()
        .checked_sub(fee)
        .ok_or(InstructionError::InsufficientFunds)?;
    payer.borrow_mut().set_lamports(new_payer_lamports);

    let new_vault_lamports = fee_vault
        .borrow()
        .lamports()
        .checked_add(fee)
        .ok_or(InstructionError::ArithmeticOverflow)?;
    fee_vault.borrow_mut().set_lamports(new_vault_lamports);

    Ok(())
}
