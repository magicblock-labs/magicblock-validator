use solana_pubkey::Pubkey;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum Error {
    // #[error("Invalid action for TransactionPreparator version: {0}")]
    // VersionError(PreparatorVersion),
    #[error("Failed to fit in single TX")]
    FailedToFitError,
    #[error("TaskBuilderError: {0}")]
    TaskBuilderError(#[from] crate::tasks::task_builder::Error),
    #[error("DeliveryPreparationError: {0}")]
    DeliveryPreparationError(
        #[from] crate::transaction_preperator::delivery_preparator::Error,
    ),
}

impl From<crate::tasks::task_strategist::Error> for Error {
    fn from(value: crate::tasks::task_strategist::Error) -> Self {
        match value {
            crate::tasks::task_strategist::Error::FailedToFitError => {
                Self::FailedToFitError
            }
        }
    }
}

pub type PreparatorResult<T, E = Error> = Result<T, E>;
