#![allow(deprecated)]
#![allow(unused_imports)]

#[rustfmt::skip]
#[path = "generated/mod.rs"]
mod generated_original;
mod utils;

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
    }
}

// This is the façade everyone should use:
pub mod generated {
    pub use crate::{
        generated_impl::*,
        generated_original::{
            accounts, errors, instructions, programs, shared,
        },
    };
}

pub use generated::{
    accounts::*,
    errors::*,
    instructions::*,
    programs::{COMPRESSED_DELEGATION_ID, COMPRESSED_DELEGATION_ID as ID},
    types::*,
    *,
};
use light_sdk::derive_light_cpi_signer;
use light_sdk_types::CpiSigner;
use solana_pubkey::{pubkey, Pubkey};
pub use utils::*;

pub const COMPRESSED_DELEGATION_CPI_SIGNER: CpiSigner =
    derive_light_cpi_signer!("DEL2rPzhFaS5qzo8XY9ZNxSzuunWueySq3p2dxJfwPbT");

pub const DEFAULT_VALIDATOR_IDENTITY: Pubkey =
    pubkey!("MAS1Dt9qreoRMQ14YQuhg8UTZMMzDdKhmkZMECCzk57");

pub const EXTERNAL_UNDELEGATE_DISCRIMINATOR: [u8; 8] =
    [0xD, 0x23, 0xB0, 0x7C, 0x70, 0x68, 0xFE, 0x73];

pub const EXTERNAL_UNDELEGATE_DISCRIMINATOR_U64: u64 =
    u64::from_le_bytes(EXTERNAL_UNDELEGATE_DISCRIMINATOR);

pub const fn id() -> Pubkey {
    ID
}
