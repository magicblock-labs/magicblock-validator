#![allow(unused)]

///
/// compat is a boundary layer for the public API.
///
/// It lets sdk expose either the legacy compatibility types or the current
/// Solana 3.0 types at the API surface, while keeping the implementation itself
/// always on Solana 3.0.
///
/// In practice, compat::{Pubkey, borsh, ...} is used only for public function
/// parameters and return types. As soon as execution enters a function body, inputs
/// are normalized to the Solana 3.0 types. with the help of AsModern and Modern traits
/// and the internal logic runs entirely on Solana 3.0. If a value needs to cross
/// back out through the public API, it is converted back at the boundary with the help
/// of Compat trait.
///

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
