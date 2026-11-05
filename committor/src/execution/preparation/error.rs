use rpc_client::MagicBlockRpcClientError;
use solana_message::CompileError;
use solana_signature::Signature;
use solana_signer::SignerError;
use thiserror::Error;
use wheels::DataLayoutError;

use crate::{
    execution::preparation::delivery_preparator::DeliveryPreparatorError,
    shared::tasks::task_strategist::TaskStrategistError,
};

#[derive(Error, Debug)]
pub enum TransactionPreparatorError {
    #[error("Failed to encode DLP instruction: {0}")]
    InstructionEncoding(#[from] DataLayoutError),
    #[error("Failed to fit in single TX")]
    FailedToFit,
    #[error("Failed to compile transaction: {0}")]
    Compile(#[from] CompileError),
    #[error("SignerError: {0}")]
    Signer(#[from] SignerError),
    #[error("Failed to get latest blockhash: {0}")]
    GetLatestBlockhash(#[source] MagicBlockRpcClientError),
    #[error("DeliveryPreparationError: {0}")]
    DeliveryPreparation(Box<DeliveryPreparatorError>),
}

impl From<DeliveryPreparatorError> for TransactionPreparatorError {
    fn from(e: DeliveryPreparatorError) -> Self {
        Self::DeliveryPreparation(Box::new(e))
    }
}

impl TransactionPreparatorError {
    pub fn signature(&self) -> Option<Signature> {
        match self {
            Self::DeliveryPreparation(err) => err.signature(),
            _ => None,
        }
    }

    pub fn is_transient(&self) -> bool {
        match self {
            Self::FailedToFit
            | Self::Compile(_)
            | Self::Signer(_)
            | Self::InstructionEncoding(_) => false,
            Self::GetLatestBlockhash(err) => err.is_transient(),
            Self::DeliveryPreparation(err) => err.is_transient(),
        }
    }
}

impl From<TaskStrategistError> for TransactionPreparatorError {
    fn from(value: TaskStrategistError) -> Self {
        match value {
            TaskStrategistError::InstructionEncoding(err) => Self::InstructionEncoding(err),
            TaskStrategistError::FailedToFit => Self::FailedToFit,
            TaskStrategistError::Compile(err) => Self::Compile(err),
            TaskStrategistError::Signer(err) => Self::Signer(err),
        }
    }
}

pub(crate) type PreparatorResult<T, E = TransactionPreparatorError> = Result<T, E>;
