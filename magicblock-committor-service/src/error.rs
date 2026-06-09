use thiserror::Error;
use tokio::sync::oneshot::error::RecvError;

use crate::intent_execution_manager::{
    intent_channerl::IntentScheduleError, IntentExecutionManagerError,
};

pub type CommittorServiceResult<T, E = CommittorServiceError> = Result<T, E>;

#[derive(Error, Debug)]
pub enum CommittorServiceError {
    #[error("IntentScheduleError: {0} ({0:?})")]
    IntentScheduleError(#[from] IntentScheduleError),

    #[error("RecvError: {0}")]
    IntentResultRecvError(#[from] RecvError),

    #[error("MagicBlockRpcClientError: {0} ({0:?})")]
    MagicBlockRpcClientError(
        #[from] magicblock_rpc_client::MagicBlockRpcClientError,
    ),

    #[error("Attempt to schedule already scheduled message id: {0}")]
    RepeatingMessageError(u64),
}
