use std::{error::Error, fmt};
pub trait PersistsAccountModData: Sync + Send + fmt::Display + 'static {
    fn persist(&self, id: u64, data: Vec<u8>) -> Result<(), Box<dyn Error>>;
    fn load(&self, id: u64) -> Result<Option<Vec<u8>>, Box<dyn Error>>;
}

/// Provides slot after which it is safe to purge slots
/// At the moment it depends on latest snapshot slot
/// but it may change in the future
pub trait FinalityProvider: Send + Sync + 'static {
    fn get_latest_final_slot(&self) -> u64;
}
