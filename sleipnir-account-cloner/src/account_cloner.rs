use conjunto_transwise::AccountChainSnapshotShared;
use futures_util::future::BoxFuture;
use sleipnir_account_fetcher::AccountFetcherError;
use sleipnir_account_updates::AccountUpdatesError;
use solana_sdk::pubkey::Pubkey;
use thiserror::Error;

#[derive(Debug, Clone, Error)]
pub enum AccountClonerError {
    #[error("SendError")]
    SendError(#[from] tokio::sync::mpsc::error::SendError<Pubkey>),

    #[error("RecvError")]
    RecvError(#[from] tokio::sync::oneshot::error::RecvError),

    #[error("TransactionError")]
    TransactionError(#[from] solana_sdk::transaction::TransactionError),

    #[error("AccountFetcherError")]
    AccountFetcherError(#[from] AccountFetcherError),

    #[error("AccountUpdatesError")]
    AccountUpdatesError(#[from] AccountUpdatesError),

    #[error("FailedToMutate '{0}'")]
    FailedToMutate(String),
}

pub type AccountClonerResult =
    Result<AccountChainSnapshotShared, AccountClonerError>;

pub trait AccountCloner {
    fn clone_account(&self, pubkey: &Pubkey) -> BoxFuture<AccountClonerResult>;
}
