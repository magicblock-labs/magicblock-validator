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

impl TryFrom<u64> for CompressedDelegationInstructionDiscriminator {
    type Error = std::io::Error;
    fn try_from(value: u64) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::InitDelegationRecord),
            1 => Ok(Self::Delegate),
            2 => Ok(Self::CommitAndFinalize),
            3 => Ok(Self::Undelegate),
            _ => Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "Invalid CompressedDelegationInstructionDiscriminator value",
            )),
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
        let disc = CompressedDelegationInstructionDiscriminator::try_from(raw)?;
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

// ------------------------------------------------------------------------------------------------
// Reimplementations of the types from light-sdk for borsh compatibility
// ------------------------------------------------------------------------------------------------

/// Reimplements [ValidityProof] compatible with borsh 1.
/// It is a wrapper around a [CompressedProof] [Option] that is compatible with borsh 1.
#[derive(
    BorshSerialize, BorshDeserialize, Clone, Copy, Debug, Default, PartialEq,
)]
pub struct CdpValidityProof(pub Option<CdpCompressedProof>);

#[derive(BorshSerialize, BorshDeserialize, Clone, Copy, Debug, PartialEq)]
pub struct CdpCompressedProof {
    pub a: [u8; 32],
    pub b: [u8; 64],
    pub c: [u8; 32],
}

impl From<CdpCompressedProof> for CompressedProof {
    fn from(cp: CdpCompressedProof) -> Self {
        Self {
            a: cp.a,
            b: cp.b,
            c: cp.c,
        }
    }
}

impl From<CompressedProof> for CdpCompressedProof {
    fn from(cp: CompressedProof) -> Self {
        Self {
            a: cp.a,
            b: cp.b,
            c: cp.c,
        }
    }
}

impl TryFrom<CdpValidityProof> for ValidityProof {
    type Error = light_compressed_account::CompressedAccountError;
    fn try_from(
        cdp_validity_proof: CdpValidityProof,
    ) -> Result<Self, Self::Error> {
        let Some(proof) = cdp_validity_proof.0 else {
            return Ok(Self(None));
        };
        let proof = CompressedProof::from(proof);
        Ok(Self(Some(proof)))
    }
}

impl From<ValidityProof> for CdpValidityProof {
    fn from(vp: ValidityProof) -> Self {
        Self(vp.0.map(CdpCompressedProof::from))
    }
}

/// Reimplements [CompressedAccountMeta] compatible with borsh 1.
/// It is a wrapper around a [CompressedAccountMeta] that is compatible with borsh 1.
#[derive(
    BorshSerialize, BorshDeserialize, Clone, Copy, Debug, PartialEq, Default,
)]
pub struct CdpCompressedAccountMeta {
    pub tree_info: CdpPackedStateTreeInfo,
    pub address: [u8; 32],
    pub output_state_tree_index: u8,
}

impl From<CompressedAccountMeta> for CdpCompressedAccountMeta {
    fn from(cam: CompressedAccountMeta) -> Self {
        Self {
            tree_info: cam.tree_info.into(),
            address: cam.address,
            output_state_tree_index: cam.output_state_tree_index,
        }
    }
}

impl From<CdpCompressedAccountMeta> for CompressedAccountMeta {
    fn from(cam: CdpCompressedAccountMeta) -> Self {
        Self {
            tree_info: cam.tree_info.into(),
            address: cam.address,
            output_state_tree_index: cam.output_state_tree_index,
        }
    }
}

#[derive(
    BorshSerialize, BorshDeserialize, Clone, Copy, Debug, PartialEq, Default,
)]
pub struct CdpPackedStateTreeInfo {
    pub root_index: u16,
    pub prove_by_index: bool,
    pub merkle_tree_pubkey_index: u8,
    pub queue_pubkey_index: u8,
    pub leaf_index: u32,
}

impl From<PackedStateTreeInfo> for CdpPackedStateTreeInfo {
    fn from(psti: PackedStateTreeInfo) -> Self {
        Self {
            root_index: psti.root_index,
            prove_by_index: psti.prove_by_index,
            merkle_tree_pubkey_index: psti.merkle_tree_pubkey_index,
            queue_pubkey_index: psti.queue_pubkey_index,
            leaf_index: psti.leaf_index,
        }
    }
}

impl From<CdpPackedStateTreeInfo> for PackedStateTreeInfo {
    fn from(psti: CdpPackedStateTreeInfo) -> Self {
        Self {
            root_index: psti.root_index,
            prove_by_index: psti.prove_by_index,
            merkle_tree_pubkey_index: psti.merkle_tree_pubkey_index,
            queue_pubkey_index: psti.queue_pubkey_index,
            leaf_index: psti.leaf_index,
        }
    }
}

/// Reimplements [PackedAddressTreeInfo] compatible with borsh 1.
/// It is a wrapper around a [PackedAddressTreeInfo] that is compatible with borsh 1.
#[derive(
    BorshSerialize, BorshDeserialize, Clone, Copy, Debug, Default, PartialEq,
)]
pub struct CdpPackedAddressTreeInfo {
    pub address_merkle_tree_pubkey_index: u8,
    pub address_queue_pubkey_index: u8,
    pub root_index: u16,
}

impl From<PackedAddressTreeInfo> for CdpPackedAddressTreeInfo {
    fn from(pati: PackedAddressTreeInfo) -> Self {
        Self {
            address_merkle_tree_pubkey_index: pati
                .address_merkle_tree_pubkey_index,
            address_queue_pubkey_index: pati.address_queue_pubkey_index,
            root_index: pati.root_index,
        }
    }
}

impl From<CdpPackedAddressTreeInfo> for PackedAddressTreeInfo {
    fn from(cdp_packed_address_tree_info: CdpPackedAddressTreeInfo) -> Self {
        Self {
            address_merkle_tree_pubkey_index: cdp_packed_address_tree_info
                .address_merkle_tree_pubkey_index,
            address_queue_pubkey_index: cdp_packed_address_tree_info
                .address_queue_pubkey_index,
            root_index: cdp_packed_address_tree_info.root_index,
        }
    }
}
