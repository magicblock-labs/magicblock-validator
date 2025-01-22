use std::error::Error;

use log::*;
use magicblock_core::traits::PersistsAccountModData;

use crate::Ledger;

impl PersistsAccountModData for Ledger {
    fn persist(&self, id: u64, data: Vec<u8>) -> Result<(), Box<dyn Error>> {
        trace!("Persisting data with id: {}, data-len: {}", id, data.len());
        self.write_account_mod_data(id, &data.into())?;
        Ok(())
    }

    fn load(&self, id: u64) -> Result<Option<Vec<u8>>, Box<dyn Error>> {
        let data = self.read_account_mod_data(id)?.map(|x| x.data);
        if log_enabled!(Level::Trace) {
            if let Some(data) = &data {
                trace!(
                    "Loading data with id: {}, data-len: {}",
                    id,
                    data.len()
                );
            } else {
                trace!("Loading data with id: {} (not found)", id);
            }
        }
        Ok(data)
    }
}
