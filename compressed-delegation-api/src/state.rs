use borsh::{BorshDeserialize, BorshSerialize};
use light_hasher::{DataHasher, Hasher, HasherError, Sha256};
use light_sdk::LightDiscriminator;
use solana_pubkey::Pubkey;

pub const DCP_DISCRIMINATOR: [u8; 8] =
    [0x4d, 0x41, 0x47, 0x49, 0x43, 0x42, 0x4c, 0x4b];

/// The Delegated Metadata includes Account Seeds, max delegation time, seeds
/// and other meta information about the delegated account.
/// * Everything necessary at cloning time is instead stored in the delegation record.
#[repr(C)]
#[derive(
    BorshSerialize,
    BorshDeserialize,
    Clone,
    Debug,
    Default,
    PartialEq,
)]
pub struct CompressedDelegationRecord {
    /// The PDA of the delegated account
    pub pda: Pubkey,
    /// The validator the account is delegated to
    pub authority: Pubkey,
    /// Last update nonce
    pub last_update_nonce: u64,
    /// Whether the account can be undelegated
    pub is_undelegatable: bool,
    /// The program that owns the delegated account
    pub owner: Pubkey,
    /// The slot at which the delegation was created
    pub delegation_slot: u64,
    /// Original lamports of the account
    pub lamports: u64,
    /// The original data of the delegated account
    pub data: Vec<u8>,
}

impl DataHasher for CompressedDelegationRecord {
    fn hash<H: Hasher>(&self) -> Result<[u8; 32], HasherError> {
        let bytes = borsh::to_vec(self).map_err(|_| HasherError::BorshError)?;
        let mut hash = Sha256::hash(&bytes)?;
        hash[0] = 0;
        Ok(hash)
    }
}

impl LightDiscriminator for CompressedDelegationRecord {
    const LIGHT_DISCRIMINATOR: [u8; 8] = DCP_DISCRIMINATOR;
    const LIGHT_DISCRIMINATOR_SLICE: &'static [u8] = &Self::LIGHT_DISCRIMINATOR;

    fn discriminator() -> [u8; 8] {
        Self::LIGHT_DISCRIMINATOR
    }
}
