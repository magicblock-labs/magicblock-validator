use solana_instruction::error::InstructionError;

use crate::{MAGIC_ACCOUNT_RENT_PER_BYTE, MAGIC_ACCOUNT_STATIC_SIZE};

/// Calculates rent for a Magic account based on its data length.
pub fn rent_for(data_len: u32) -> Result<u64, InstructionError> {
    let total_size = u64::from(data_len)
        .checked_add(MAGIC_ACCOUNT_STATIC_SIZE)
        .ok_or(InstructionError::ArithmeticOverflow)?;
    total_size
        .checked_mul(MAGIC_ACCOUNT_RENT_PER_BYTE)
        .ok_or(InstructionError::ArithmeticOverflow)
}
