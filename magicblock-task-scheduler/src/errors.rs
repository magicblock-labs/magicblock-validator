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

    #[error(transparent)]
    Transaction(#[from] solana_sdk::transaction::TransactionError),

    #[error("Task context not found")]
    TaskContextNotFound,

    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error("Failed to process some context requests: {0:?}")]
    SchedulingRequests(Vec<TaskSchedulerError>),

    #[error("Failed to serialize task context: {0:?}")]
    ContextSerialization(Vec<u8>),

    #[error("Failed to deserialize task context: {0:?}")]
    ContextDeserialization(Vec<u8>),
}
