use std::io;

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
    Internal(&'static str),
}

impl From<lmdb::Error> for AccountsDbError {
    fn from(error: lmdb::Error) -> Self {
        match error {
            lmdb::Error::NotFound => Self::NotFound,
            other => Self::Lmdb(other),
        }
    }
}

#[macro_export]
macro_rules! inspecterr {
    ($msg: expr) => {
        |err| ::log::warn!("{} error: {err}", $msg)
    };
    ($msg: expr, $($ctx:expr),* $(,)?) => {
        |err| ::log::warn!("{} error: {err}", format!($msg, $($ctx),*))
    };
}
