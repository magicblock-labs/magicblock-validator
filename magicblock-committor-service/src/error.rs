use solana_pubkey::Pubkey;
use solana_signature::Signature;
use solana_transaction_error::TransactionError;
use thiserror::Error;
use tokio::sync::oneshot::error::RecvError;

use crate::{
    intent_execution_manager::IntentExecutionManagerError,
    intent_executor::task_info_fetcher::TaskInfoFetcherError,
};

pub type CommittorServiceResult<T, E = CommittorServiceError> = Result<T, E>;

#[derive(Error, Debug)]
pub enum CommittorServiceError {
    #[error("CommitPersistError: {0} ({0:?})")]
    CommitPersistError(#[from] crate::persist::error::CommitPersistError),

    // #[error("MagicBlockRpcClientError: {0} ({0:?})")]
    // MagicBlockRpcClientError(
    //     #[from] magicblock_rpc_client::MagicBlockRpcClientError,
    // ),

    // #[error("TableManiaError: {0} ({0:?})")]
    // TableManiaError(#[from] magicblock_table_mania::error::TableManiaError),
    #[error("IntentExecutionManagerError: {0} ({0:?})")]
    IntentExecutionManagerError(#[from] IntentExecutionManagerError),

    #[error("RecvError: {0}")]
    IntentResultRecvError(#[from] RecvError),

    // #[error("TaskInfoFetcherError: {0} ({0:?})")]
    // TaskInfoFetcherError(#[from] TaskInfoFetcherError),

    // #[error("Task {0} failed to compile transaction message: {1} ({1:?})")]
    // FailedToCompileTransactionMessage(String, solana_message::CompileError),

    // #[error("Task {0} failed to create transaction: {1} ({1:?})")]
    // FailedToCreateTransaction(String, solana_signer::SignerError),
    #[error("Attempt to schedule already scheduled message id: {0}")]
    RepeatingMessageError(u64),
}

impl CommittorServiceError {
    pub fn signature(&self) -> Option<Signature> {
        use CommittorServiceError::*;
        match self {
            // MagicBlockRpcClientError(e) => e.signature(),
            _ => None,
        }
    }
}
