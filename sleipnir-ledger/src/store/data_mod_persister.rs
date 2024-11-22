use crate::Ledger;
use log::*;
use sleipnir_core::traits::PersistsAccountModData;
use std::error::Error;

impl PersistsAccountModData for Ledger {
    fn persist(&self, id: u64, data: Vec<u8>) -> Result<(), Box<dyn Error>> {
        info!("Persisting data with id: {}, data-len: {}", id, data.len());
        self.write_account_mod_data(id, &data.into())?;
        Ok(())
    }

    fn load(&self, id: u64) -> Result<Option<Vec<u8>>, Box<dyn Error>> {
        info!("Loading data with id: {}", id);
        let data = self.read_account_mod_data(id)?.map(|x| x.data);
        Ok(data)
    }
}
