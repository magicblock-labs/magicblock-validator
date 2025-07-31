use thiserror::Error;

#[derive(Error, Debug)]
pub enum TaskSchedulerError {
    #[error("Database error: {0}")]
    DatabaseConnection(#[from] r2d2::Error),

    #[error("Database error: {0}")]
    Database(#[from] r2d2_sqlite::rusqlite::Error),

    #[error("Failed to remove database file: {0}")]
    RemoveDatabaseFile(#[from] std::io::Error),

    #[error("Pubsub error: {0}")]
    Pubsub(
        #[from]
        solana_pubsub_client::nonblocking::pubsub_client::PubsubClientError,
    ),

    #[error("JSON serialization error: {0}")]
    SerdeJson(#[from] serde_json::Error),

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
