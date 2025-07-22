use solana_pubkey::Pubkey;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum Error {
    // #[error("Invalid action for TransactionPreparator version: {0}")]
    // VersionError(PreparatorVersion),
    #[error("Failed to fit in single TX")]
    FailedToFitError,
    #[error("Missing commit id for pubkey: {0}")]
    MissingCommitIdError(Pubkey),
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

impl From<crate::tasks::task_builder::Error> for Error {
    fn from(value: crate::tasks::task_builder::Error) -> Self {
        match value {
            crate::tasks::task_builder::Error::MissingCommitIdError(pubkey) => {
                Self::MissingCommitIdError(pubkey)
            }
        }
    }
}

pub type PreparatorResult<T, E = Error> = Result<T, E>;
