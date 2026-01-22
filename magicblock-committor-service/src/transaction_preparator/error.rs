use solana_signature::Signature;
use solana_signer::SignerError;
use thiserror::Error;

use crate::{
    tasks::task_strategist::TaskStrategistError,
    transaction_preparator::delivery_preparator::DeliveryPreparatorError,
};

#[derive(Error, Debug)]
pub enum TransactionPreparatorError {
    #[error("Failed to fit in single TX")]
    FailedToFitError,
    #[error("Inconsistent tasks compression used in strategy")]
    InconsistentTaskCompression,
    #[error("SignerError: {0}")]
    SignerError(#[from] SignerError),
    #[error("DeliveryPreparationError: {0}")]
    DeliveryPreparationError(Box<DeliveryPreparatorError>),
}

impl From<DeliveryPreparatorError> for TransactionPreparatorError {
    fn from(e: DeliveryPreparatorError) -> Self {
        Self::DeliveryPreparationError(Box::new(e))
    }
}

impl TransactionPreparatorError {
    pub fn signature(&self) -> Option<Signature> {
        match self {
            Self::DeliveryPreparationError(err) => err.signature(),
            _ => None,
        }
    }
}

impl From<TaskStrategistError> for TransactionPreparatorError {
    fn from(value: TaskStrategistError) -> Self {
        match value {
            TaskStrategistError::FailedToFitError => Self::FailedToFitError,
            TaskStrategistError::SignerError(err) => Self::SignerError(err),
            TaskStrategistError::InconsistentTaskCompression => {
                Self::InconsistentTaskCompression
            }
        }
    }
}

pub type PreparatorResult<T, E = TransactionPreparatorError> = Result<T, E>;
