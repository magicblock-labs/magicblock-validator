//! Ephemeral account instruction processors
//!
//! Ephemeral accounts are zero-balance accounts with rent paid by a sponsor.
//! Rent is charged at 32 lamports/byte (109x cheaper than Solana base rent).

mod process_close;
mod process_create;
mod process_resize;
mod validation;

use std::cell::RefCell;

use magicblock_magic_program_api::EPHEMERAL_RENT_PER_BYTE;
pub(crate) use process_close::process_close_ephemeral_account;
pub(crate) use process_create::process_create_ephemeral_account;
pub(crate) use process_resize::process_resize_ephemeral_account;
use solana_account::{AccountSharedData, ReadableAccount};
use solana_instruction::error::InstructionError;
use solana_transaction_context::TransactionContext;

use crate::utils::accounts;

// ----- Account indices shared by validation and processors -----

/// Instruction account index for the sponsor (rent payer).
const SPONSOR_IDX: u16 = 0;
/// Instruction account index for the ephemeral account.
const EPHEMERAL_IDX: u16 = 1;
/// Instruction account index for the vault.
const VAULT_IDX: u16 = 2;

// ----- Shared helpers -----

/// Maximum allowed data length for ephemeral accounts (10 MB, matching Solana's limit)
const MAX_DATA_LEN: u32 = 10 * 1024 * 1024;

/// Calculates rent for an ephemeral account based on its data length.
fn rent_for(data_len: u32) -> Result<u64, InstructionError> {
    let total_size = u64::from(data_len)
        .checked_add(AccountSharedData::ACCOUNT_STATIC_SIZE as u64)
        .ok_or(InstructionError::ArithmeticOverflow)?;
    total_size
        .checked_mul(EPHEMERAL_RENT_PER_BYTE)
        .ok_or(InstructionError::ArithmeticOverflow)
}

/// Returns the data length of an ephemeral account as a `u32`.
fn get_ephemeral_data_len(
    ephemeral: &RefCell<AccountSharedData>,
) -> Result<u32, InstructionError> {
    ephemeral
        .borrow()
        .data()
        .len()
        .try_into()
        .map_err(|_| InstructionError::ArithmeticOverflow)
}

/// Transfers rent between sponsor and vault.
///
/// Positive `amount` moves lamports from sponsor to vault (creation / growth).
/// Negative `amount` moves lamports from vault to sponsor (close / shrink).
fn transfer_rent(
    tc: &TransactionContext,
    amount: i64,
) -> Result<(), InstructionError> {
    if amount > 0 {
        let abs = amount as u64;
        accounts::debit_instruction_account_at_index(tc, SPONSOR_IDX, abs)?;
        accounts::credit_instruction_account_at_index(tc, VAULT_IDX, abs)?;
    } else {
        let abs = amount.unsigned_abs();
        accounts::credit_instruction_account_at_index(tc, SPONSOR_IDX, abs)?;
        accounts::debit_instruction_account_at_index(tc, VAULT_IDX, abs)?;
    }
    Ok(())
}
