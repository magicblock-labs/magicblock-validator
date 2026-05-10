use std::io::{Read, Write};

use borsh::{BorshDeserialize, BorshSerialize};
use light_sdk::instruction::{
    account_meta::CompressedAccountMeta, CompressedProof,
    PackedAddressTreeInfo, PackedStateTreeInfo, ValidityProof,
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
pub enum CompressedDelegationProgramInstruction {
    /// Initialize a new delegation record.
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
    pub validity_proof: CdpValidityProof,
    /// Address tree info
    pub address_tree_info: CdpPackedAddressTreeInfo,
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
    pub validity_proof: CdpValidityProof,
    /// Account meta
    pub account_meta: CdpCompressedAccountMeta,
    /// Owner program id
    pub owner_program_id: Pubkey,
    /// Validator
    pub validator: Pubkey,
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
    pub account_meta: CdpCompressedAccountMeta,
    /// Validity proof
    pub validity_proof: CdpValidityProof,
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
    pub validity_proof: CdpValidityProof,
    /// Delegation record account meta
    pub delegation_record_account_meta: CdpCompressedAccountMeta,
    /// Compressed delegated record
    pub compressed_delegated_record: CompressedDelegationRecord,
}

#[derive(BorshSerialize, BorshDeserialize, Clone, Debug, PartialEq)]
pub struct ExternalUndelegateArgs {
    /// The delegation record
    pub delegation_record: CompressedDelegationRecord,
}

/// Reimplementations of the types from light-sdk for borsh compatibility

#[derive(
    BorshSerialize, BorshDeserialize, Clone, Copy, Debug, Default, PartialEq,
)]
pub struct CdpValidityProof(pub Option<[u8; 128]>);

impl TryFrom<CdpValidityProof> for ValidityProof {
    type Error = light_compressed_account::CompressedAccountError;
    fn try_from(
        cdp_validity_proof: CdpValidityProof,
    ) -> Result<Self, Self::Error> {
        let Some(bytes) = cdp_validity_proof.0 else {
            return Ok(Self(None));
        };
        let proof = CompressedProof::try_from(&bytes[..])?;
        Ok(Self(Some(proof)))
    }
}

impl From<ValidityProof> for CdpValidityProof {
    fn from(vp: ValidityProof) -> Self {
        Self(vp.0.map(|proof| proof.to_array()))
    }
}

#[derive(BorshSerialize, BorshDeserialize, Clone, Copy, Debug, PartialEq)]
pub struct CdpCompressedAccountMeta(pub [u8; 42]);

impl From<CompressedAccountMeta> for CdpCompressedAccountMeta {
    fn from(cam: CompressedAccountMeta) -> Self {
        let mut bytes = [0u8; 42];
        bytes[0..2].copy_from_slice(&cam.tree_info.root_index.to_le_bytes());
        bytes[2] = cam.tree_info.prove_by_index as u8;
        bytes[3] = cam.tree_info.merkle_tree_pubkey_index;
        bytes[4] = cam.tree_info.queue_pubkey_index;
        bytes[5..9].copy_from_slice(&cam.tree_info.leaf_index.to_le_bytes());
        bytes[9..41].copy_from_slice(&cam.address);
        bytes[41] = cam.output_state_tree_index;
        Self(bytes)
    }
}

impl TryFrom<CdpCompressedAccountMeta> for CompressedAccountMeta {
    type Error = core::array::TryFromSliceError;
    fn try_from(cam: CdpCompressedAccountMeta) -> Result<Self, Self::Error> {
        Ok(Self {
            tree_info: PackedStateTreeInfo {
                root_index: u16::from_le_bytes(cam.0[0..2].try_into()?),
                prove_by_index: cam.0[2] != 0,
                merkle_tree_pubkey_index: cam.0[3],
                queue_pubkey_index: cam.0[4],
                leaf_index: u32::from_le_bytes(cam.0[5..9].try_into()?),
            },
            address: cam.0[9..41].try_into()?,
            output_state_tree_index: cam.0[41],
        })
    }
}

impl Default for CdpCompressedAccountMeta {
    fn default() -> Self {
        Self([0u8; 42])
    }
}

#[derive(
    BorshSerialize, BorshDeserialize, Clone, Copy, Debug, Default, PartialEq,
)]
pub struct CdpPackedAddressTreeInfo(pub [u8; 4]);

impl From<PackedAddressTreeInfo> for CdpPackedAddressTreeInfo {
    fn from(pati: PackedAddressTreeInfo) -> Self {
        let mut bytes = [0u8; 4];
        bytes[0..1].copy_from_slice(
            &pati.address_merkle_tree_pubkey_index.to_le_bytes(),
        );
        bytes[1..2]
            .copy_from_slice(&pati.address_queue_pubkey_index.to_le_bytes());
        bytes[2..4].copy_from_slice(&pati.root_index.to_le_bytes());
        Self(bytes)
    }
}

impl TryFrom<CdpPackedAddressTreeInfo> for PackedAddressTreeInfo {
    type Error = core::array::TryFromSliceError;
    fn try_from(
        cdp_packed_address_tree_info: CdpPackedAddressTreeInfo,
    ) -> Result<Self, Self::Error> {
        Ok(Self {
            address_merkle_tree_pubkey_index: cdp_packed_address_tree_info.0[0],
            address_queue_pubkey_index: cdp_packed_address_tree_info.0[1],
            root_index: u16::from_le_bytes(
                cdp_packed_address_tree_info.0[2..4].try_into()?,
            ),
        })
    }
}
