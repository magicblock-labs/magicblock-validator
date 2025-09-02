use thiserror::Error;

#[derive(Error, Debug)]
pub enum TaskSchedulerError {
    #[error(transparent)]
    DatabaseConnection(#[from] rusqlite::Error),

    #[error("Pubsub error: {0}")]
    Pubsub(
        #[from]
        solana_pubsub_client::nonblocking::pubsub_client::PubsubClientError,
    ),

    #[error("Bincode serialization error: {0}")]
    Bincode(#[from] bincode::Error),

    #[error("RPC error: {0}")]
    Rpc(String),

    #[error("Task not found: {0}")]
    TaskNotFound(u64),

    #[error("Invalid task data: {0}")]
    InvalidTaskData(String),

    #[error("Task execution failed: {0}")]
    TaskExecution(String),

    #[error("Failed to sanitize transactions: {0}")]
    SanitizeTransactions(String),

    #[error("Task context not found")]
    TaskContextNotFound,

    #[error("Failed to execute transaction: {0}")]
    ExecuteTransaction(#[from] solana_sdk::transaction::TransactionError),
}
