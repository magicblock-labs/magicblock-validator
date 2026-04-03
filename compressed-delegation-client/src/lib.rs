pub mod builders;
pub mod cpi;
mod programs;

use light_sdk::derive_light_cpi_signer;
use light_sdk_types::CpiSigner;
use solana_pubkey::Pubkey;
pub use {
    compressed_delegation_api::{
        CommitAndFinalizeArgs, CompressedDelegationInstructionDiscriminator,
        CompressedDelegationProgramInstruction, CompressedDelegationRecord,
        DelegateArgs, ExternalUndelegateArgs, InitDelegationRecordArgs,
        UndelegateArgs, DCP_DISCRIMINATOR, EXTERNAL_UNDELEGATE_DISCRIMINATOR,
        EXTERNAL_UNDELEGATE_DISCRIMINATOR_U64,
    },
    programs::{COMPRESSED_DELEGATION_ID, COMPRESSED_DELEGATION_ID as ID},
};

/// Backwards-compatible name for [`CompressedDelegationInstructionDiscriminator`].
pub type DelegationProgramDiscriminator =
    CompressedDelegationInstructionDiscriminator;

pub const COMPRESSED_DELEGATION_CPI_SIGNER: CpiSigner =
    derive_light_cpi_signer!("DEL2rPzhFaS5qzo8XY9ZNxSzuunWueySq3p2dxJfwPbT");

pub const fn id() -> Pubkey {
    ID
}
