use std::io;

use log::error;

#[derive(Debug, thiserror::Error)]
pub enum AccountsDbError {
    #[error("requested account doesn't exist in adb")]
    NotFound,
    #[error("io error during adb access: {0}")]
    Io(#[from] io::Error),
    #[error("lmdb index error: {0}")]
    Lmdb(lmdb::Error),
    #[error("snapshot for slot {0} doesn't exist")]
    SnapshotMissing(u64),
    #[error("internal accountsdb error: {0}")]
    Internal(String),
}

impl From<lmdb::Error> for AccountsDbError {
    fn from(error: lmdb::Error) -> Self {
        match error {
            lmdb::Error::NotFound => Self::NotFound,
            err => Self::Lmdb(err),
        }
    }
}

/// Extension trait to easily log errors in Result chains.
pub trait LogErr<T, E> {
    /// Logs the error if the result is `Err`, then returns the result unmodified.
    fn log_err<F, S>(self, msg: F) -> Result<T, E>
    where
        F: FnOnce() -> S,
        S: std::fmt::Display;
}

impl<T, E> LogErr<T, E> for Result<T, E>
where
    E: std::fmt::Display,
{
    #[track_caller]
    fn log_err<F, S>(self, msg: F) -> Result<T, E>
    where
        F: FnOnce() -> S,
        S: std::fmt::Display,
    {
        if let Err(e) = &self {
            error!("{}: {}", msg(), e);
        }
        self
    }
}
