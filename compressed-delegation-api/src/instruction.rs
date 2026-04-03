use std::io::{Read, Write};

use borsh::{BorshDeserialize, BorshSerialize};
use light_sdk::instruction::{
    account_meta::CompressedAccountMeta, PackedAddressTreeInfo, ValidityProof,
};
use solana_pubkey::Pubkey;

use crate::state::CompressedDelegationRecord;

/// Instruction discriminators for the compressed delegation program (`u64`).
#[repr(u64)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompressedDelegationInstructionDiscriminator {
    InitDelegationRecord = 0,
    Delegate = 1,
    CommitAndFinalize = 2,
    Undelegate = 3,
}

impl CompressedDelegationInstructionDiscriminator {
    pub fn from_u64(value: u64) -> Option<Self> {
        match value {
            0 => Some(Self::InitDelegationRecord),
            1 => Some(Self::Delegate),
            2 => Some(Self::CommitAndFinalize),
            3 => Some(Self::Undelegate),
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
#[repr(u64)]
pub enum CompressedDelegationProgramInstruction {
    /// Delegate the compressed account to a validator.
    InitDelegationRecord { args: InitDelegationRecordArgs },

    /// Delegate the compressed account to a validator.
    Delegate { args: DelegateArgs },

    /// Commit the compressed account.
    CommitAndFinalize { args: CommitAndFinalizeArgs },

    /// Undelegate the compressed account.
    Undelegate { args: UndelegateArgs },
}

impl CompressedDelegationProgramInstruction {
    /// Discriminator for this instruction (first `u64` in serialized form).
    pub fn discriminator(
        &self,
    ) -> CompressedDelegationInstructionDiscriminator {
        match self {
            Self::InitDelegationRecord { .. } => {
                CompressedDelegationInstructionDiscriminator::InitDelegationRecord
            }
            Self::Delegate { .. } => CompressedDelegationInstructionDiscriminator::Delegate,
            Self::CommitAndFinalize { .. } => CompressedDelegationInstructionDiscriminator::CommitAndFinalize,
            Self::Undelegate { .. } => CompressedDelegationInstructionDiscriminator::Undelegate,
        }
    }
}

impl BorshSerialize for CompressedDelegationProgramInstruction {
    fn serialize<W: Write>(&self, writer: &mut W) -> std::io::Result<()> {
        (self.discriminator() as u64).serialize(writer)?;
        match self {
            Self::InitDelegationRecord { args } => args.serialize(writer),
            Self::Delegate { args } => args.serialize(writer),
            Self::CommitAndFinalize { args } => args.serialize(writer),
            Self::Undelegate { args } => args.serialize(writer),
        }
    }
}

impl BorshDeserialize for CompressedDelegationProgramInstruction {
    fn deserialize_reader<R: Read>(reader: &mut R) -> std::io::Result<Self> {
        let raw = u64::deserialize_reader(reader)?;
        let disc = CompressedDelegationInstructionDiscriminator::from_u64(raw)
            .ok_or_else(|| {
                std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "Invalid CompressedDelegationProgramInstruction discriminant",
            )
            })?;
        Ok(match disc {
            CompressedDelegationInstructionDiscriminator::InitDelegationRecord => {
                Self::InitDelegationRecord {
                    args: InitDelegationRecordArgs::deserialize_reader(reader)?,
                }
            }
            CompressedDelegationInstructionDiscriminator::Delegate => Self::Delegate {
                args: DelegateArgs::deserialize_reader(reader)?,
            },
            CompressedDelegationInstructionDiscriminator::CommitAndFinalize => Self::CommitAndFinalize {
                args: CommitAndFinalizeArgs::deserialize_reader(reader)?,
            },
            CompressedDelegationInstructionDiscriminator::Undelegate => Self::Undelegate {
                args: UndelegateArgs::deserialize_reader(reader)?,
            },
        })
    }
}

#[repr(C)]
#[derive(
    BorshSerialize, BorshDeserialize, Clone, Default, Debug, PartialEq,
)]
pub struct InitDelegationRecordArgs {
    /// The proof of the account data
    pub validity_proof: ValidityProof,
    /// Address tree info
    pub address_tree_info: PackedAddressTreeInfo,
    /// Output state tree index
    pub output_state_tree_index: u8,
    /// Owner program id
    pub owner_program_id: Pubkey,
    /// PDA seeds
    pub pda_seeds: Vec<Vec<u8>>,
    /// Bump
    pub bump: u8,
}

#[repr(C)]
#[derive(
    BorshSerialize, BorshDeserialize, Clone, Default, Debug, PartialEq,
)]
pub struct DelegateArgs {
    /// The proof of the account data
    pub validity_proof: ValidityProof,
    /// Account meta
    pub account_meta: CompressedAccountMeta,
    /// Owner program id
    pub owner_program_id: Pubkey,
    /// Validator
    pub validator: Pubkey,
    /// Original lamports of the account
    pub lamports: u64,
    /// Account data before delegation
    pub account_data: Vec<u8>,
    /// PDA seeds
    pub pda_seeds: Vec<Vec<u8>>,
    /// Bump
    pub bump: u8,
}

#[repr(C)]
#[derive(
    BorshSerialize, BorshDeserialize, Clone, Default, Debug, PartialEq,
)]
pub struct CommitAndFinalizeArgs {
    /// The current data of the compressed delegated account
    pub current_compressed_delegated_account_data: Vec<u8>,
    /// The new state of the compressed delegated account data
    pub new_data: Vec<u8>,
    /// Compressed account meta
    pub account_meta: CompressedAccountMeta,
    /// Validity proof
    pub validity_proof: ValidityProof,
    /// Update nonce
    pub update_nonce: u64,
    /// Allow undelegation
    pub allow_undelegation: bool,
}

#[derive(
    BorshSerialize, BorshDeserialize, Clone, Default, Debug, PartialEq,
)]
pub struct UndelegateArgs {
    /// The proof of the account data
    pub validity_proof: ValidityProof,
    /// Delegation record account meta
    pub delegation_record_account_meta: CompressedAccountMeta,
    /// Compressed delegated record
    pub compressed_delegated_record: CompressedDelegationRecord,
}

#[derive(BorshSerialize, BorshDeserialize, Clone, Debug, PartialEq)]
pub struct ExternalUndelegateArgs {
    /// The delegation record
    pub delegation_record: CompressedDelegationRecord,
}
