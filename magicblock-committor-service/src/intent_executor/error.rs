use magicblock_rpc_client::MagicBlockRpcClientError;
use solana_sdk::signature::{Signature, SignerError};

#[derive(thiserror::Error, Debug)]
pub enum InternalError {
    #[error("SignerError: {0}")]
    SignerError(#[from] SignerError),
    #[error("MagicBlockRpcClientError: {0}")]
    MagicBlockRpcClientError(#[from] MagicBlockRpcClientError),
}

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("EmptyIntentError")]
    EmptyIntentError,
    #[error("Failed to fit in single TX")]
    FailedToFitError,
    // TODO: remove once proper retries introduced
    #[error("TaskBuilderError: {0}")]
    TaskBuilderError(#[from] crate::tasks::task_builder::Error),
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
    FailedCommitPreparationError(
        #[source] crate::transaction_preparator::error::Error,
    ),
    #[error("FailedFinalizePreparationError: {0}")]
    FailedFinalizePreparationError(
        #[source] crate::transaction_preparator::error::Error,
    ),
}

impl From<crate::tasks::task_strategist::Error> for Error {
    fn from(value: crate::tasks::task_strategist::Error) -> Self {
        let crate::tasks::task_strategist::Error::FailedToFitError = value;
        Self::FailedToFitError
    }
}

pub type IntentExecutorResult<T, E = Error> = Result<T, E>;
