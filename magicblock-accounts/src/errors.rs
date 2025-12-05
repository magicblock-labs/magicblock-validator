use std::collections::HashSet;

use magicblock_account_cloner::AccountClonerError;
use magicblock_committor_service::{
    error::CommittorServiceError, service_ext::CommittorServiceExtError,
    ChangesetMeta,
};
use solana_pubkey::Pubkey;
use solana_transaction_error::TransactionError;
use thiserror::Error;
use tokio::sync::oneshot::error::RecvError;

pub type AccountsResult<T> = std::result::Result<T, AccountsError>;

#[derive(Error, Debug)]
pub enum AccountsError {
    #[error("UrlParseError: {0}")]
    UrlParseError(#[from] Box<url::ParseError>),

    #[error("TransactionError: {0}")]
    TransactionError(#[from] Box<TransactionError>),

    #[error("CommittorSerivceError: {0}")]
    CommittorSerivceError(#[from] CommittorServiceError),

    #[error("CommittorServiceExtError: {0}")]
    CommittorServiceExtError(#[from] CommittorServiceExtError),

    #[error("TokioOneshotRecvError")]
    TokioOneshotRecvError(#[from] Box<tokio::sync::oneshot::error::RecvError>),

    #[error("AccountClonerError")]
    AccountClonerError(#[from] AccountClonerError),

    #[error("InvalidRpcUrl '{0}'")]
    InvalidRpcUrl(String),

    #[error("FailedToUpdateUrlScheme")]
    FailedToUpdateUrlScheme,

    #[error("FailedToUpdateUrlPort")]
    FailedToUpdateUrlPort,

    #[error("FailedToGetLatestBlockhash '{0}'")]
    FailedToGetLatestBlockhash(String),

    #[error("FailedToGetReimbursementAddress '{0}'")]
    FailedToGetReimbursementAddress(String),

    #[error("FailedToSendCommitTransaction '{0}'")]
    FailedToSendCommitTransaction(String, HashSet<Pubkey>, HashSet<Pubkey>),

    #[error("Too many committees: {0}")]
    TooManyCommittees(usize),

    #[error("FailedToObtainReqidForCommittedChangeset {0:?}")]
    FailedToObtainReqidForCommittedChangeset(Box<ChangesetMeta>),
}

#[derive(Error, Debug)]
pub enum ScheduledCommitsProcessorError {
    #[error("RecvError: {0}")]
    RecvError(#[from] RecvError),
    #[error("CommittorSerivceError")]
    CommittorSerivceError(Box<CommittorServiceError>),
}

impl From<CommittorServiceError> for ScheduledCommitsProcessorError {
    fn from(e: CommittorServiceError) -> Self {
        Self::CommittorSerivceError(Box::new(e))
    }
}

pub type ScheduledCommitsProcessorResult<
    T,
    E = ScheduledCommitsProcessorError,
> = Result<T, E>;
