use thiserror::Error;
use crate::tasks::task_strategist::TaskStrategistError;

#[derive(Error, Debug)]
pub enum TransactionPreparatorError {
    #[error("Failed to fit in single TX")]
    FailedToFitError,
    #[error("DeliveryPreparationError: {0}")]
    DeliveryPreparationError(
        #[from] crate::transaction_preparator::delivery_preparator::Error,
    ),
}

impl From<TaskStrategistError>
    for TransactionPreparatorError
{
    fn from(value: TaskStrategistError) -> Self {
        match value {
            TaskStrategistError::FailedToFitError => {
                Self::FailedToFitError
            }
        }
    }
}

pub type PreparatorResult<T, E = TransactionPreparatorError> = Result<T, E>;
