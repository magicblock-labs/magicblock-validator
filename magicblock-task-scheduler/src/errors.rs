use solana_program::example_mocks::solana_rpc_client_api;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum TaskSchedulerError {
    #[error(transparent)]
    DatabaseConnection(#[from] rusqlite::Error),

    #[error(transparent)]
    Pubsub(
        #[from]
        solana_pubsub_client::nonblocking::pubsub_client::PubsubClientError,
    ),

    #[error(transparent)]
    Bincode(#[from] bincode::Error),

    #[error(transparent)]
    Rpc(#[from] solana_rpc_client_api::client_error::ClientError),

    #[error("Task not found: {0}")]
    TaskNotFound(u64),

    #[error("Invalid task data: {0}")]
    InvalidTaskData(String),

    #[error(transparent)]
    Transaction(#[from] solana_sdk::transaction::TransactionError),

    #[error("Task context not found")]
    TaskContextNotFound,

    #[error(transparent)]
    Io(#[from] std::io::Error),
}
