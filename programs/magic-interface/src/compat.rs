//! Reexports of the current Solana types used by this API.

pub use solana_program::{
    account_info::AccountInfo,
    declare_id,
    instruction::{AccountMeta, Instruction},
    pubkey,
    pubkey::Pubkey,
};
pub use solana_signature::Signature;
