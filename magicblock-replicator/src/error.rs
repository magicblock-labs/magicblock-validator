//! Error types for the replication protocol.

use std::fmt::{Debug, Display};

use magicblock_ledger::errors::LedgerError;
use solana_transaction_error::TransactionError;

/// Replication operation errors.
#[derive(thiserror::Error, Debug)]
pub enum Error {
    /// NATS message broker error.
    #[error("message broker error: {0}")]
    Nats(async_nats::Error),

    /// I/O operation failed.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// Serialization or deserialization failed.
    #[error("serialization error: {0}")]
    SerDe(#[from] bincode::Error),

    /// Ledger access error.
    #[error("ledger access error: {0}")]
    Ledger(#[from] LedgerError),

    /// Transaction execution error.
    #[error("transaction execution error: {0}")]
    Transaction(#[from] TransactionError),

    /// Internal protocol violation or malformed data.
    #[error("internal error: {0}")]
    Internal(String),

    /// File system watcher error.
    #[error("watcher error: {0}")]
    Watcher(#[from] notify::Error),
}

// async_nats::Error is actually async_nats::error::Error<K> where K is the error kind.
// We need this generic impl to convert all variants.
impl<K> From<async_nats::error::Error<K>> for Error
where
    K: Display + Debug + Clone + PartialEq + Sync + Send + 'static,
{
    fn from(value: async_nats::error::Error<K>) -> Self {
        Self::Nats(value.into())
    }
}

/// Convenience alias for `Result<T, Error>`.
pub type Result<T> = std::result::Result<T, Error>;
