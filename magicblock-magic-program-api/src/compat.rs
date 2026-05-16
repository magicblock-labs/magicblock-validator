//#[cfg(not(any(feature = "latest", feature = "backward-compat")))]
//compile_error!("enable either the `latest` or `backward-compat` feature");

#[cfg(feature = "backward-compat")]
mod selected {
    pub use solana_program_compat::{
        account_info::AccountInfo,
        declare_id,
        instruction::{AccountMeta, Instruction},
        pubkey,
        pubkey::Pubkey,
    };
    pub use solana_signature_compat::Signature;
}

#[cfg(not(feature = "backward-compat"))]
mod selected {
    pub use solana_program::{
        account_info::AccountInfo,
        declare_id,
        instruction::{AccountMeta, Instruction},
        pubkey,
        pubkey::Pubkey,
    };
    pub use solana_signature::Signature;
}

pub use selected::*;
