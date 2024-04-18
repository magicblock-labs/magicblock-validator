use thiserror::Error;

pub type BlockstoreResult<T> = std::result::Result<T, BlockstoreError>;

#[derive(Error, Debug)]
pub enum BlockstoreError {
    #[error("RocksDB error: {0}")]
    RocksDb(#[from] rocksdb::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("fs extra error: {0}")]
    FsExtraError(#[from] fs_extra::error::Error),
    #[error("serialization error: {0}")]
    Serialize(#[from] Box<bincode::ErrorKind>),
}
