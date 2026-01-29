//! Core processor utilities for ephemeral account instructions

use magicblock_magic_program_api::EPHEMERAL_RENT_PER_BYTE;
use solana_account::AccountSharedData;
use solana_instruction::error::InstructionError;

/// Maximum allowed data length for ephemeral accounts (10 MB, matching Solana's limit)
pub(crate) const MAX_DATA_LEN: u32 = 10 * 1024 * 1024;

/// Calculates rent for an ephemeral account based on its data length.
pub(crate) fn rent_for(data_len: u32) -> Result<u64, InstructionError> {
    let total_size = u64::from(data_len)
        .checked_add(AccountSharedData::ACCOUNT_STATIC_SIZE as u64)
        .ok_or(InstructionError::ArithmeticOverflow)?;
    total_size
        .checked_mul(EPHEMERAL_RENT_PER_BYTE)
        .ok_or(InstructionError::ArithmeticOverflow)
}
