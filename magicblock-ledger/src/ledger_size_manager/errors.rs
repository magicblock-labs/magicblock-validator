use thiserror::Error;
use tokio::task::JoinError;

#[derive(Error, Debug)]
pub enum LedgerSizeManagerError {
    #[error(transparent)]
    LedgerError(#[from] crate::errors::LedgerError),
    #[error("Failed to join worker: {0}")]
    JoinError(#[from] JoinError),
}
pub type LedgerSizeManagerResult<T> = Result<T, LedgerSizeManagerError>;
