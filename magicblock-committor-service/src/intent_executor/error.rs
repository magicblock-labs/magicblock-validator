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
        commit_signature: Signature,
        finalize_signature: Option<Signature>,
    },
    #[error("FailedCommitPreparationError: {0}")]
    FailedCommitPreparationError(
        #[source] crate::transaction_preperator::error::Error,
    ),
    #[error("FailedFinalizePreparationError: {0}")]
    FailedFinalizePreparationError(
        #[source] crate::transaction_preperator::error::Error,
    ),
}

pub type IntentExecutorResult<T, E = Error> = Result<T, E>;
