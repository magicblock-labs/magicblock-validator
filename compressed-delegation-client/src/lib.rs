//! A workaround crate used while the validator is incompatible with solana 2.3
//! TODO: remove this once the validator is compatible with solana 2.3
mod common;
#[rustfmt::skip]
#[path = "generated/mod.rs"]
mod generated_original;

pub mod generated_impl {
    pub mod types {
        pub use light_compressed_account::instruction_data::{
            data::OutputCompressedAccountWithPackedContext,
            with_readonly::InAccount,
        };
        pub use light_sdk::instruction::{
            account_meta::CompressedAccountMeta, PackedAddressTreeInfo,
            ValidityProof,
        };

        pub use super::super::generated_original::{
            accounts::CompressedDelegationRecord, types::*,
        };
        pub use crate::common::{
            OptionalCompressedAccountMeta, OptionalPackedAddressTreeInfo,
        };
    }
}

// This is the façade everyone should use:
pub mod generated {
    pub use crate::{
        generated_impl::*,
        generated_original::{accounts::*, instructions::*},
    };
}

pub use generated::*;
use solana_pubkey::Pubkey;

pub const ID: Pubkey =
    Pubkey::from_str_const("DEL2rPzhFaS5qzo8XY9ZNxSzuunWueySq3p2dxJfwPbT");

pub use ID as COMPRESSED_DELEGATION_ID;

pub const fn id() -> Pubkey {
    ID
}
