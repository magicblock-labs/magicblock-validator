use std::sync::Arc;

use crate::persist::CommitStrategy;
use magicblock_rpc_client::MagicBlockRpcClientError;
use solana_pubkey::Pubkey;
use solana_sdk::signature::Signature;
use thiserror::Error;

use crate::CommitInfo;

pub type CommittorServiceResult<T> =
    std::result::Result<T, CommittorServiceError>;

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
        solana_rpc_client_api::client_error::Error,
    ),

    #[error("Failed to confirm init changeset account: {0} ({0:?})")]
    FailedToConfirmInitChangesetAccount(
        solana_rpc_client_api::client_error::Error,
    ),
    #[error("Init transaction '{0}' was not confirmed")]
    InitChangesetAccountNotConfirmed(String),

    #[error("Task {0} failed to compile transaction message: {1} ({1:?})")]
    FailedToCompileTransactionMessage(
        String,
        solana_sdk::message::CompileError,
    ),

    #[error("Task {0} failed to creqate transaction: {1} ({1:?})")]
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

pub type CommitAccountResult<T> = std::result::Result<T, CommitAccountError>;
#[derive(Error, Debug)]
/// Specific error that always includes the commit info
pub enum CommitAccountError {
    #[error("Failed to init buffer and chunk account: {0}")]
    InitBufferAndChunkAccounts(String, Box<CommitInfo>, CommitStrategy),

    #[error("Failed to get chunks account: ({0:?})")]
    GetChunksAccount(
        Option<MagicBlockRpcClientError>,
        Arc<CommitInfo>,
        CommitStrategy,
    ),

    #[error("Failed to deserialize chunks account: {0} ({0:?})")]
    DeserializeChunksAccount(std::io::Error, Arc<CommitInfo>, CommitStrategy),

    #[error("Failed to write complete chunks of commit data after max retries. Last write error {0:?}")]
    WriteChunksRanOutOfRetries(
        Option<magicblock_rpc_client::MagicBlockRpcClientError>,
        Arc<CommitInfo>,
        CommitStrategy,
    ),
}

impl CommitAccountError {
    pub fn into_commit_info(self) -> CommitInfo {
        use CommitAccountError::*;
        let ci = match self {
            InitBufferAndChunkAccounts(_, commit_info, _) => {
                return *commit_info;
            }
            GetChunksAccount(_, commit_info, _) => commit_info,
            DeserializeChunksAccount(_, commit_info, _) => commit_info,
            WriteChunksRanOutOfRetries(_, commit_info, _) => commit_info,
        };
        Arc::<CommitInfo>::unwrap_or_clone(ci)
    }
}
