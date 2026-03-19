use std::cell::RefCell;

use solana_account::{AccountSharedData, ReadableAccount, WritableAccount};
use solana_instruction::error::InstructionError;
use solana_pubkey::Pubkey;

pub mod account_actions;
pub mod accounts;
pub(crate) mod instruction_context_frames;
pub mod instruction_utils;

// NOTE: there is no low level SDK currently that exposes the program address
//       we hardcode it here to avoid either having to pull in the delegation program
//       or a higher level SDK including procmacros for CPI, etc.
pub const DELEGATION_PROGRAM_ID: Pubkey =
    Pubkey::from_str_const("DELeGGvXpWV2fqJUhqcF5ZSYMS4JTLjteaAMARRSaeSh");

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
