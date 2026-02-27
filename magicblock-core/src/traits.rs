use std::{error::Error, fmt};

use solana_clock::Clock;
use solana_hash::Hash;

use crate::Slot;

pub trait PersistsAccountModData: Sync + Send + fmt::Display + 'static {
    fn persist(&self, id: u64, data: Vec<u8>) -> Result<(), Box<dyn Error>>;
    fn load(&self, id: u64) -> Result<Option<Vec<u8>>, Box<dyn Error>>;
}

/// Provides read access to the latest confirmed block's metadata.
/// Allows components to access block data without depending on the full ledger,
/// abstracting away the underlying storage.
pub trait LatestBlockProvider: Send + Sync + Clone + 'static {
    fn slot(&self) -> Slot;
    fn blockhash(&self) -> Hash;
    fn clock(&self) -> Clock;
}
