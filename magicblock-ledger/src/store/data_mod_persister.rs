use std::error::Error;

use magicblock_core::traits::PersistsAccountModData;
use tracing::*;

use crate::Ledger;

impl PersistsAccountModData for Ledger {
    fn persist(&self, id: u64, data: Vec<u8>) -> Result<(), Box<dyn Error>> {
        trace!(id, data_len = data.len(), "Persisting data");
        self.write_account_mod_data(id, &data.into())?;
        Ok(())
    }

    fn load(&self, id: u64) -> Result<Option<Vec<u8>>, Box<dyn Error>> {
        let data = self.read_account_mod_data(id)?.map(|x| x.data);
        if enabled!(Level::TRACE) {
            if let Some(data) = &data {
                trace!(id, data_len = data.len(), "Loading data");
            } else {
                trace!(id, found = false, "Loading data");
            }
        }
        Ok(data)
    }
}
