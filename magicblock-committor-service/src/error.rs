use thiserror::Error;
use tokio::sync::oneshot::error::RecvError;

use crate::intent_execution_manager::IntentExecutionManagerError;

pub type CommittorServiceResult<T, E = CommittorServiceError> = Result<T, E>;

#[derive(Error, Debug)]
pub enum CommittorServiceError {
    #[error("CommitPersistError: {0} ({0:?})")]
    CommitPersistError(#[from] crate::persist::error::CommitPersistError),

    #[error("IntentExecutionManagerError: {0} ({0:?})")]
    IntentExecutionManagerError(#[from] IntentExecutionManagerError),

    #[error("RecvError: {0}")]
    IntentResultRecvError(#[from] RecvError),

    #[error("MagicBlockRpcClientError: {0} ({0:?})")]
    MagicBlockRpcClientError(
        #[from] magicblock_rpc_client::MagicBlockRpcClientError,
    ),

    #[error("Attempt to schedule already scheduled message id: {0}")]
    RepeatingMessageError(u64),
}
