use solana_pubkey::Pubkey;
use solana_sdk::signature::Signature;
use thiserror::Error;

use crate::intent_execution_manager::IntentExecutionManagerError;

pub type CommittorServiceResult<T, E = CommittorServiceError> = Result<T, E>;

#[derive(Error, Debug)]
pub enum CommittorServiceError {
    #[error("CommittorError: {0} ({0:?})")]
    CommittorError(#[from] magicblock_committor_program::error::CommittorError),

    #[error("CommitPersistError: {0} ({0:?})")]
    CommitPersistError(#[from] crate::persist::error::CommitPersistError),

    #[error("MagicBlockRpcClientError: {0} ({0:?})")]
    MagicBlockRpcClientError(
        #[from] magicblock_rpc_client::MagicBlockRpcClientError,
    ),

    #[error("TableManiaError: {0} ({0:?})")]
    TableManiaError(#[from] magicblock_table_mania::error::TableManiaError),

    #[error("IntentExecutionManagerError: {0} ({0:?})")]
    IntentExecutionManagerError(#[from] IntentExecutionManagerError),

    #[error(
        "Failed send and confirm transaction to {0} on chain: {1} ({1:?})"
    )]
    FailedToSendAndConfirmTransaction(
        String,
        magicblock_rpc_client::MagicBlockRpcClientError,
    ),

    #[error("The transaction to {0} was sent and confirmed, but encountered an error: {1} ({1:?})")]
    EncounteredTransactionError(
        String,
        solana_sdk::transaction::TransactionError,
    ),

    #[error("Failed to send init changeset account: {0} ({0:?})")]
    FailedToSendInitChangesetAccount(
        Box<solana_rpc_client_api::client_error::Error>,
    ),

    #[error("Failed to confirm init changeset account: {0} ({0:?})")]
    FailedToConfirmInitChangesetAccount(
        Box<solana_rpc_client_api::client_error::Error>,
    ),
    #[error("Init transaction '{0}' was not confirmed")]
    InitChangesetAccountNotConfirmed(String),

    #[error("Task {0} failed to compile transaction message: {1} ({1:?})")]
    FailedToCompileTransactionMessage(
        String,
        solana_sdk::message::CompileError,
    ),

    #[error("Task {0} failed to create transaction: {1} ({1:?})")]
    FailedToCreateTransaction(String, solana_sdk::signer::SignerError),

    #[error("Could not find commit strategy for bundle {0}")]
    CouldNotFindCommitStrategyForBundle(u64),

    #[error("Failed to fetch metadata account for {0}")]
    FailedToFetchDelegationMetadata(Pubkey),

    #[error("Failed to deserialize metadata account for {0}, {1:?}")]
    FailedToDeserializeDelegationMetadata(
        Pubkey,
        solana_sdk::program_error::ProgramError,
    ),
}

impl CommittorServiceError {
    pub fn signature(&self) -> Option<Signature> {
        use CommittorServiceError::*;
        match self {
            MagicBlockRpcClientError(e) => e.signature(),
            FailedToSendAndConfirmTransaction(_, e) => e.signature(),
            _ => None,
        }
    }
}
