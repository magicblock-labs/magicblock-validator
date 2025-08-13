use std::{error::Error, fmt};
pub trait PersistsAccountModData: Sync + Send + fmt::Display + 'static {
    fn persist(&self, id: u64, data: Vec<u8>) -> Result<(), Box<dyn Error>>;
    fn load(&self, id: u64) -> Result<Option<Vec<u8>>, Box<dyn Error>>;
}
