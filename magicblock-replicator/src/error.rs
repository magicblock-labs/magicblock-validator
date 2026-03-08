//! Error types for the replication protocol.

use std::fmt::{Debug, Display};

/// Replication operation errors.
#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("message broker error: {0}")]
    Nats(async_nats::Error),
    #[error("connection closed")]
    ConnectionClosed,
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("serialization error: {0}")]
    SerDe(#[from] bincode::Error),
}

impl<K> From<async_nats::error::Error<K>> for Error
where
    K: Display + Debug + Clone + PartialEq + Sync + Send + 'static,
{
    fn from(value: async_nats::error::Error<K>) -> Self {
        Self::Nats(value.into())
    }
}

pub type Result<T> = std::result::Result<T, Error>;
