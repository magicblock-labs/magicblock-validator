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

/// Charges `amount` lamports from a delegated payer to a recipient.
///
/// Returns `InvalidAccountData` if the payer is not delegated,
/// `InsufficientFunds` if it cannot cover the fee.
pub(crate) fn charge_delegated_payer(
    payer: &RefCell<AccountSharedData>,
    recipient: &RefCell<AccountSharedData>,
    amount: u64,
) -> Result<(), InstructionError> {
    if !payer.borrow().delegated() {
        return Err(InstructionError::InvalidAccountData);
    }
    let payer_lamports = payer.borrow().lamports();
    if payer_lamports < amount {
        return Err(InstructionError::InsufficientFunds);
    }
    payer.borrow_mut().set_lamports(payer_lamports - amount);
    let recipient_lamports = recipient.borrow().lamports();
    recipient
        .borrow_mut()
        .set_lamports(recipient_lamports.saturating_add(amount));
    Ok(())
}

/// Sets proper account values during undelegation
pub(crate) fn mark_account_as_undelegated(acc: &RefCell<AccountSharedData>) {
    set_account_owner(acc, DELEGATION_PROGRAM_ID);
    let mut acc = acc.borrow_mut();
    acc.set_undelegating(true);
    acc.set_delegated(false);
}
