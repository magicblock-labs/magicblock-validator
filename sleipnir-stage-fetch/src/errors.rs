// NOTE: from core/src/result.rs with all errors removed that we don't use here
use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error(transparent)]
    RecvTimeout(#[from] crossbeam_channel::RecvTimeoutError),
    #[error(transparent)]
    Recv(#[from] crossbeam_channel::RecvError),
    #[error("Send")]
    Send,
}

pub type Result<T> = std::result::Result<T, Error>;
