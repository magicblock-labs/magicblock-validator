use std::{error::Error, fmt};

use solana_program::instruction::InstructionError;

use crate::intent::CommittedAccount;

pub const NONCE_LIMIT_ERR: u32 = 0xA000_0000;

pub trait MagicSys: Sync + Send + 'static {
    fn persist(&self, id: u64, data: Vec<u8>) -> Result<(), Box<dyn Error>>;
    fn load(&self, id: u64) -> Result<Option<Vec<u8>>, Box<dyn Error>>;

    fn validate_commits(
        &self,
        pubkeys: &[CommittedAccount],
    ) -> Result<(), InstructionError>;
}
