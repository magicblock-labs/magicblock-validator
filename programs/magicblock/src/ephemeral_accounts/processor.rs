//! Core processor utilities for ephemeral account instructions

use magicblock_magic_program_api::EPHEMERAL_RENT_PER_BYTE;
use solana_account::AccountSharedData;

/// Calculates rent for an ephemeral account based on its data length
pub(crate) const fn rent_for(data_len: usize) -> u64 {
    let total_size = data_len as u64 + AccountSharedData::ACCOUNT_STATIC_SIZE as u64;
    total_size * EPHEMERAL_RENT_PER_BYTE
}
