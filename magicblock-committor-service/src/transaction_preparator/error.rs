use thiserror::Error;

#[derive(Error, Debug)]
pub enum TransactionPreparatorError {
    #[error("Failed to fit in single TX")]
    FailedToFitError,
    #[error("DeliveryPreparationError: {0}")]
    DeliveryPreparationError(
        #[from] crate::transaction_preparator::delivery_preparator::Error,
    ),
}

impl From<crate::tasks::task_strategist::TaskStrategistError>
    for TransactionPreparatorError
{
    fn from(value: crate::tasks::task_strategist::TaskStrategistError) -> Self {
        match value {
            crate::tasks::task_strategist::TaskStrategistError::FailedToFitError => {
                Self::FailedToFitError
            }
        }
    }
}

pub type PreparatorResult<T, E = TransactionPreparatorError> = Result<T, E>;
