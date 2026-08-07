use solana_transaction::InstructionError;
use thiserror::Error;

pub type TaskSchedulerResult<T> = Result<T, TaskSchedulerError>;

#[derive(Error, Debug)]
pub enum TaskSchedulerError {
    #[error(transparent)]
    Instruction(#[from] InstructionError),

    #[error("Invalid configuration: {0}")]
    InvalidConfiguration(String),

    #[error(transparent)]
    Wincode(#[from] wincode::WriteError),

    #[error("Transaction execution failed: {0}")]
    TransactionExecution(String),

    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error("Task {0} already exists and is owned by {1}, not {2}")]
    UnauthorizedReplacing(i64, String, String),

    #[error("Batch size mismatch: expected {0}, got {1}")]
    SizeMismatch(usize, usize),

    #[error(transparent)]
    RpcClient(#[from] solana_rpc_client::api::client_error::Error),
}
