use thiserror::Error;

use crate::transaction_preperator::transaction_preparator::PreparatorVersion;

#[derive(Error, Debug)]
pub enum Error {
    #[error("Invalid action for version: {0}")]
    VersionError(PreparatorVersion),
    #[error("Failed to fit in single TX")]
    FailedToFitError,
    #[error("InternalError: {0}")]
    InternalError(#[from] anyhow::Error),
}

pub type PreparatorResult<T, E = Error> = Result<T, E>;
