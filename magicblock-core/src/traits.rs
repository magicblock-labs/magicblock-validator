use std::{error::Error, fmt};

use solana_program::instruction::InstructionError;
use solana_pubkey::Pubkey;

pub trait MagicSys: Sync + Send + fmt::Display + 'static {
    fn persist(&self, id: u64, data: Vec<u8>) -> Result<(), Box<dyn Error>>;
    fn load(&self, id: u64) -> Result<Option<Vec<u8>>, Box<dyn Error>>;
    fn validate_commit_nonces(
        &self,
        pubkeys: &[Pubkey],
    ) -> Result<(), InstructionError>;
}
