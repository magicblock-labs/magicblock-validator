use crate::Ledger;
use log::*;
use sleipnir_core::traits::PersistsAccountModData;
use std::error::Error;

impl PersistsAccountModData for Ledger {
    fn persist(&self, id: usize, data: &[u8]) -> Result<(), Box<dyn Error>> {
        info!("Persisting data with id: {}, data-len: {}", id, data.len());
        Ok(())
    }

    fn load(&self, id: usize) -> Result<Option<Vec<u8>>, Box<dyn Error>> {
        info!("Loading data with id: {}", id);
        Ok(None)
    }
}
