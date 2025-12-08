use thiserror::Error;

pub type TaskSchedulerResult<T> = Result<T, TaskSchedulerError>;

#[derive(Error, Debug)]
pub enum TaskSchedulerError {
    #[error(transparent)]
    DatabaseConnection(#[from] rusqlite::Error),

    #[error(transparent)]
    Pubsub(
        Box<
            solana_pubsub_client::nonblocking::pubsub_client::PubsubClientError,
        >,
    ),

    #[error(transparent)]
    Bincode(#[from] bincode::Error),

    #[error("Task not found: {0}")]
    TaskNotFound(i64),

    #[error(transparent)]
    Transaction(#[from] solana_transaction_error::TransactionError),

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

    #[error("Task {0} already exists and is owned by {1}, not {2}")]
    UnauthorizedReplacing(i64, String, String),
}

impl From<solana_pubsub_client::nonblocking::pubsub_client::PubsubClientError>
    for TaskSchedulerError
{
    fn from(
        e: solana_pubsub_client::nonblocking::pubsub_client::PubsubClientError,
    ) -> Self {
        Self::Pubsub(Box::new(e))
    }
}
