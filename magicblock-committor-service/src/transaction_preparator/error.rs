use thiserror::Error;

#[derive(Error, Debug)]
pub enum Error {
    #[error("Failed to fit in single TX")]
    FailedToFitError,
    #[error("DeliveryPreparationError: {0}")]
    DeliveryPreparationError(
        #[from] crate::transaction_preparator::delivery_preparator::Error,
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
