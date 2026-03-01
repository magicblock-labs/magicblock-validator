//! Error types for the replication protocol.

/// Replication operation errors.
#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("connection closed")]
    ConnectionClosed,
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("serialization error: {0}")]
    SerDe(#[from] bincode::Error),
}

pub type Result<T> = std::result::Result<T, Error>;
