use conjunto_transwise::AccountChainSnapshotShared;
use futures_util::future::BoxFuture;
use solana_sdk::pubkey::Pubkey;
use thiserror::Error;

#[derive(Debug, Clone, Error)]
pub enum AccountClonerError {
    #[error("SendError")]
    SendError(#[from] tokio::sync::mpsc::error::SendError<Pubkey>),

    #[error("RecvError")]
    RecvError(#[from] tokio::sync::oneshot::error::RecvError),

    #[error("FailedToClone '{0}'")]
    FailedToClone(String),
}

pub type AccountCloneResult = Result<(), AccountClonerError>;

pub trait AccountCloner {
    fn clone_account(&self, pubkey: &Pubkey) -> BoxFuture<AccountCloneResult>;
}
