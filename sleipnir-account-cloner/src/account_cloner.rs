use conjunto_transwise::AccountChainSnapshotShared;
use futures_util::future::BoxFuture;
use sleipnir_account_dumper::AccountDumperError;
use sleipnir_account_fetcher::AccountFetcherError;
use sleipnir_account_updates::AccountUpdatesError;
use solana_sdk::pubkey::Pubkey;
use thiserror::Error;
use tokio::sync::oneshot::Sender;

#[derive(Debug, Clone, Error)]
pub enum AccountClonerError {
    #[error(transparent)]
    SendError(#[from] tokio::sync::mpsc::error::SendError<Pubkey>),

    #[error(transparent)]
    RecvError(#[from] tokio::sync::oneshot::error::RecvError),

    #[error(transparent)]
    AccountFetcherError(#[from] AccountFetcherError),

    #[error(transparent)]
    AccountUpdatesError(#[from] AccountUpdatesError),

    #[error(transparent)]
    AccountDumperError(#[from] AccountDumperError),

    #[error("ProgramDataDoesNotExist")]
    ProgramDataDoesNotExist,
}

pub type AccountClonerResult<T> = Result<T, AccountClonerError>;

pub type AccountClonerListeners =
    Vec<Sender<AccountClonerResult<AccountClonerOutput>>>;

#[derive(Debug, Clone)]
pub enum AccountClonerOutput {
    Cloned(AccountChainSnapshotShared),
    Skipped,
}

pub trait AccountCloner {
    fn clone_account(
        &self,
        pubkey: &Pubkey,
    ) -> BoxFuture<AccountClonerResult<AccountClonerOutput>>;
}
