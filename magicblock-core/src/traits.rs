use std::{collections::HashMap, error::Error};

use solana_program::instruction::InstructionError;
use solana_pubkey::Pubkey;

use crate::intent::CommittedAccount;

/// Trait that provides access to system calls implemented outside of SVM,
/// accessible in magic-program.
pub trait MagicSys: Sync + Send + 'static {
    fn persist(&self, id: u64, data: Vec<u8>) -> Result<(), Box<dyn Error>>;
    fn load(&self, id: u64) -> Result<Option<Vec<u8>>, Box<dyn Error>>;

    fn fetch_current_commit_nonces(
        &self,
        commits: &[CommittedAccount],
    ) -> Result<HashMap<Pubkey, u64>, InstructionError>;
}
