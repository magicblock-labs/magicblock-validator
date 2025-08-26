use solana_sdk::signer::SignerError;
use thiserror::Error;

use crate::tasks::task_strategist::TaskStrategistError;

#[derive(Error, Debug)]
pub enum TransactionPreparatorError {
    #[error("Failed to fit in single TX")]
    FailedToFitError,
    #[error("SignerError: {0}")]
    SignerError(#[from] SignerError),
    #[error("DeliveryPreparationError: {0}")]
    DeliveryPreparationError(
        #[from] crate::transaction_preparator::delivery_preparator::Error,
    ),
}

impl From<TaskStrategistError> for TransactionPreparatorError {
    fn from(value: TaskStrategistError) -> Self {
        match value {
            TaskStrategistError::FailedToFitError => Self::FailedToFitError,
            TaskStrategistError::SignerError(err) => Self::SignerError(err),
        }
    }
}

pub type PreparatorResult<T, E = TransactionPreparatorError> = Result<T, E>;
