#![allow(unused)]

// compat is a boundary layer for the public API.
//
// It lets the SDK expose either the legacy compatibility types or current
// Solana types at the API surface.

#[cfg(feature = "backward-compat")]
mod backward_compat {
    pub use solana_program_compat::{
        account_info::AccountInfo,
        declare_id,
        instruction::{AccountMeta, Instruction},
        pubkey,
        pubkey::Pubkey,
    };
    pub use solana_signature_compat::Signature;
}

mod latest {
    pub use solana_program::{
        account_info::AccountInfo,
        declare_id,
        instruction::{AccountMeta, Instruction},
        pubkey,
        pubkey::Pubkey,
    };
    pub use solana_signature::Signature;
}

#[cfg(feature = "backward-compat")]
pub use backward_compat::*;
#[cfg(not(feature = "backward-compat"))]
pub use latest::*;
