use thiserror::Error;

pub type TaskSchedulerResult<T> = Result<T, TaskSchedulerError>;

#[derive(Error, Debug)]
pub enum TaskSchedulerError {
    #[error(transparent)]
    Keeper(#[from] keeper::error::KeeperError),

    #[error(transparent)]
    RpcClient(#[from] solana_rpc_client::api::client_error::Error),
}
