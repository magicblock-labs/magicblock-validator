use magicblock_rpc_client::MagicBlockRpcClientError;
use solana_sdk::signature::{Signature, SignerError};
use solana_sdk::transaction::TransactionError;
use crate::{
    tasks::{
        task_builder::TaskBuilderError, task_strategist::TaskStrategistError,
    },
    transaction_preparator::error::TransactionPreparatorError,
};

#[derive(thiserror::Error, Debug)]
pub enum InternalError {
    #[error("SignerError: {0}")]
    SignerError(#[from] SignerError),
    #[error("MagicBlockRpcClientError: {0}")]
    MagicBlockRpcClientError(#[from] MagicBlockRpcClientError),
}

#[derive(thiserror::Error, Debug)]
pub enum IntentExecutorError {
    #[error("EmptyIntentError")]
    EmptyIntentError,
    #[error("Failed to fit in single TX")]
    FailedToFitError,
    // TODO: remove once proper retries introduced
    #[error("TaskBuilderError: {0}")]
    TaskBuilderError(#[from] TaskBuilderError),
    #[error("FailedToCommitError: {err}")]
    FailedToCommitError {
        #[source]
        err: InternalError,
        signature: Option<Signature>,
    },
    #[error("FailedToFinalizeError: {err}")]
    FailedToFinalizeError {
        #[source]
        err: InternalError,
        commit_signature: Option<Signature>,
        finalize_signature: Option<Signature>,
    },
    #[error("FailedCommitPreparationError: {0}")]
    FailedCommitPreparationError(#[source] TransactionPreparatorError),
    #[error("FailedFinalizePreparationError: {0}")]
    FailedFinalizePreparationError(#[source] TransactionPreparatorError),
}

/// Those are the errors that may occur during Commit/Finalize stages on Base layer
#[derive(thiserror::Error, Debug)]
pub enum TransactionStrategyExecutionError {
    #[error("User supplied action are ill-formed!")]
    ActionsError,
    #[error("Accounts committed with an invalid Commit id")]
    CommitIDError,
    #[error("Max instruction trace length exceeded")]
    CpiLimitError,
    #[error("InternalError: {0}")]
    InternalError(#[source] InternalError),
}

impl From<&TransactionError> for TransactionStrategyExecutionError {
    match
}

impl From<TaskStrategistError> for IntentExecutorError {
    fn from(value: TaskStrategistError) -> Self {
        let TaskStrategistError::FailedToFitError = value;
        Self::FailedToFitError
    }
}

pub type IntentExecutorResult<T, E = IntentExecutorError> = Result<T, E>;
