//! Shared instruction layouts, discriminators, and account types for the
//! compressed delegation program. Used by the on-chain program and off-chain clients.

pub mod instruction;
pub mod state;

pub use instruction::{
    CommitAndFinalizeArgs, CompressedDelegationInstructionDiscriminator,
    CompressedDelegationProgramInstruction, DelegateArgs,
    ExternalUndelegateArgs, InitDelegationRecordArgs, UndelegateArgs,
};
pub use state::{CompressedDelegationRecord, DCP_DISCRIMINATOR};

/// Discriminator for CPI into owner programs for external undelegate.
pub const EXTERNAL_UNDELEGATE_DISCRIMINATOR: [u8; 8] =
    [0xD, 0x23, 0xB0, 0x7C, 0x70, 0x68, 0xFE, 0x73];

pub const EXTERNAL_UNDELEGATE_DISCRIMINATOR_U64: u64 =
    u64::from_le_bytes(EXTERNAL_UNDELEGATE_DISCRIMINATOR);
