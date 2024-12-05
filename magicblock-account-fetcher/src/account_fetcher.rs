use conjunto_transwise::AccountChainSnapshotShared;
use futures_util::future::BoxFuture;
use solana_sdk::{clock::Slot, pubkey::Pubkey};
use thiserror::Error;
use tokio::sync::oneshot::Sender;

#[derive(Debug, Clone, Error)]
pub enum AccountFetcherError {
    #[error(transparent)]
    SendError(
        #[from] tokio::sync::mpsc::error::SendError<(Pubkey, Option<Slot>)>,
    ),

    #[error(transparent)]
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
        min_context_slot: Option<Slot>,
    ) -> BoxFuture<AccountFetcherResult<AccountChainSnapshotShared>>;
}
