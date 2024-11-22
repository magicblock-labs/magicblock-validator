use std::error::Error;
pub trait PersistsAccountModData: Sync + Send + 'static {
    fn persist(&self, id: usize, data: &[u8]) -> Result<(), Box<dyn Error>>;
    fn load(&self, id: usize) -> Result<Option<Vec<u8>>, Box<dyn Error>>;
}
