use thiserror::Error;

pub type TaskSchedulerResult<T> = Result<T, TaskSchedulerError>;

#[derive(Error, Debug)]
pub enum TaskSchedulerError {
    #[error(transparent)]
    DatabaseConnection(#[from] rusqlite::Error),

    #[error(transparent)]
    Bincode(#[from] bincode::Error),

    #[error(transparent)]
    Rpc(#[from] Box<solana_rpc_client_api::client_error::Error>),

    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error("Task {0} already exists and is owned by {1}, not {2}")]
    UnauthorizedReplacing(i64, String, String),
}
