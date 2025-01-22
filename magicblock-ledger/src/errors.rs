use thiserror::Error;

pub type LedgerResult<T> = std::result::Result<T, LedgerError>;

#[derive(Error, Debug)]
pub enum LedgerError {
    #[error("RocksDB error: {0}")]
    RocksDb(#[from] rocksdb::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("fs extra error: {0}")]
    FsExtraError(#[from] fs_extra::error::Error),
    #[error("serialization error: {0}")]
    Serialize(#[from] Box<bincode::ErrorKind>),
    #[error("protobuf encode error: {0}")]
    ProtobufEncodeError(#[from] prost::EncodeError),
    #[error("protobuf decode error: {0}")]
    ProtobufDecodeError(#[from] prost::DecodeError),
    #[error("AccountsDb error: {0}")]
    AccountsDbError(#[from] magicblock_accounts_db::errors::AccountsDbError),
    #[error("unable to set open file descriptor limit")]
    UnableToSetOpenFileDescriptorLimit,
    #[error("transaction not found")]
    TransactionNotFound,
    #[error("transaction status meta not found")]
    TransactionStatusMetaNotFound,
    #[error("transaction status slot mismatch")]
    TransactionStatusSlotMismatch,
    #[error("transaction-index overflow")]
    TransactionIndexOverflow,
    #[error("Failed to convert transaction {0}")]
    TransactionConversionError(String),
    #[error("slot cleaned up")]
    SlotCleanedUp,
    #[error("try from slice error: {0}")]
    TryFromSliceError(#[from] std::array::TryFromSliceError),
    #[error("BlockstoreProcessorError: {0}")]
    BlockStoreProcessor(String),
}
