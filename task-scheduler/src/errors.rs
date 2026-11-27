use thiserror::Error;

pub type TaskSchedulerResult<T> = Result<T, TaskSchedulerError>;

#[derive(Error, Debug)]
pub enum TaskSchedulerError {
    #[error(transparent)]
    Keeper(#[from] keeper::error::KeeperError),

    #[error(transparent)]
    Instruction(#[from] solana_instruction::error::InstructionError),

    #[error(
        "task {task_id} does not fit a hydra crank: a scheduled instruction has \
         {accounts} accounts (max {max_accounts}) and {data_len} data bytes (max {max_data_len})"
    )]
    CrankTooLarge {
        task_id: i64,
        accounts: usize,
        max_accounts: usize,
        data_len: usize,
        max_data_len: usize,
    },

    #[error(transparent)]
    RpcClient(#[from] solana_rpc_client::api::client_error::Error),
}
