use light_hasher::{DataHasher, Hasher, HasherError, Sha256};

use crate::generated::accounts::CompressedDelegationRecord;

pub const DCP_DISCRIMINATOR: [u8; 8] =
    [0x4d, 0x41, 0x47, 0x49, 0x43, 0x42, 0x4c, 0x4b];

impl DataHasher for CompressedDelegationRecord {
    fn hash<H: Hasher>(&self) -> Result<[u8; 32], HasherError> {
        let bytes = borsh::to_vec(self).map_err(|_| HasherError::BorshError)?;
        let mut hash = Sha256::hash(bytes.as_slice())?;
        hash[0] = 0;
        Ok(hash)
    }
}
