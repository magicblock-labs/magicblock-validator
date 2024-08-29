use conjunto_transwise::AccountChainSnapshotShared;
use futures_util::future::BoxFuture;
use solana_sdk::pubkey::Pubkey;
use thiserror::Error;
use tokio::sync::oneshot::Sender;

#[derive(Debug, Clone, Error)]
pub enum AccountFetcherError {
    #[error("SendError")]
    SendError(#[from] tokio::sync::mpsc::error::SendError<Pubkey>),

    #[error("RecvError")]
    RecvError(#[from] tokio::sync::oneshot::error::RecvError),

    #[error("FailedToFetch '{0}'")]
    FailedToFetch(String),
}

pub type AccountFetcherResult<T> = Result<T, AccountFetcherError>;

pub type AccountFetcherListeners =
    Vec<Sender<AccountFetcherResult<AccountChainSnapshotShared>>>;

pub trait AccountFetcher {
    fn fetch_account_chain_snapshot(
        &self,
        pubkey: &Pubkey,
    ) -> BoxFuture<AccountFetcherResult<AccountChainSnapshotShared>>;
}
