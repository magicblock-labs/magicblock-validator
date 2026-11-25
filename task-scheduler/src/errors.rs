use solana_pubkey::Pubkey;
use thiserror::Error;

pub type TaskSchedulerResult<T> = Result<T, TaskSchedulerError>;

#[derive(Error, Debug)]
pub enum TaskSchedulerError {
    #[error(transparent)]
    Keeper(#[from] keeper::error::KeeperError),

    #[error(transparent)]
    Instruction(#[from] solana_instruction::error::InstructionError),

    #[error(transparent)]
    RpcClient(#[from] solana_rpc_client::api::client_error::Error),

    /// A live account at the crank address is not owned by hydra.
    #[error("account {crank} at crank address is owned by {owner}, not hydra")]
    UnexpectedCrankOwner { crank: Pubkey, owner: Pubkey },
}
