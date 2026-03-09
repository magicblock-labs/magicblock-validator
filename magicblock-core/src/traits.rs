use std::{collections::HashMap, error::Error};

use solana_clock::Clock;
use solana_hash::Hash;
use solana_program::instruction::InstructionError;
use solana_pubkey::Pubkey;

use crate::{intent::CommittedAccount, Slot};

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

/// Provides read access to the latest confirmed block's metadata.
/// Allows components to access block data without depending on the full ledger,
/// abstracting away the underlying storage.
pub trait LatestBlockProvider: Send + Sync + Clone + 'static {
    fn slot(&self) -> Slot;
    fn blockhash(&self) -> Hash;
    fn clock(&self) -> Clock;
}
