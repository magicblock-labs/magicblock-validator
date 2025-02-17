use std::io;

#[derive(Debug, thiserror::Error)]
pub enum AdbError {
    #[error("requested account doesn't exist in adb")]
    NotFound,
    #[error("io error during adb access: {0}")]
    Io(#[from] io::Error),
    #[error("lmdb index error: {0}")]
    Lmdb(lmdb::Error),
    #[error("snapshot for slot {0} doesn't exist")]
    SnapshotMissing(u64),
}

impl From<lmdb::Error> for AdbError {
    fn from(error: lmdb::Error) -> Self {
        match error {
            lmdb::Error::NotFound => Self::NotFound,
            other => Self::Lmdb(other),
        }
    }
}
