use solana_instruction::error::InstructionError;

use crate::{EPHEMERAL_ACCOUNT_STATIC_SIZE, EPHEMERAL_RENT_PER_BYTE};

/// Calculates rent for an ephemeral account based on its data length.
pub fn rent_for(data_len: u32) -> Result<u64, InstructionError> {
    let total_size = u64::from(data_len)
        .checked_add(EPHEMERAL_ACCOUNT_STATIC_SIZE)
        .ok_or(InstructionError::ArithmeticOverflow)?;
    total_size
        .checked_mul(EPHEMERAL_RENT_PER_BYTE)
        .ok_or(InstructionError::ArithmeticOverflow)
}
