use thiserror::Error;

pub type BlockstoreResult<T> = std::result::Result<T, BlockstoreError>;

#[derive(Error, Debug)]
pub enum BlockstoreError {
    #[error("RocksDB error: {0}")]
    RocksDb(#[from] rocksdb::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}
