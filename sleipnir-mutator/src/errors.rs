use solana_sdk::pubkey::Pubkey;
use thiserror::Error;

pub type MutatorResult<T> = std::result::Result<T, MutatorError>;

#[derive(Error, Debug)]
pub enum MutatorError {
    #[error("ParsePubkeyError: '{0}' ({0:?})")]
    ParsePubkeyError(#[from] solana_sdk::pubkey::ParsePubkeyError),

    #[error("RpcClientError: '{0}' ({0:?})")]
    RpcClientError(#[from] solana_rpc_client_api::client_error::Error),

    #[error("StdError: '{0}' ({0:?})")]
    StdError(#[from] Box<dyn std::error::Error>),

    #[error(transparent)]
    InstructionError(#[from] solana_sdk::instruction::InstructionError),

    #[error(transparent)]
    PubkeyError(#[from] solana_sdk::pubkey::PubkeyError),

    #[error("Invalid cluster '{0}'")]
    InvalidCluster(String),

    #[error("Bank forks not set")]
    BankForksNotSet,

    #[error("Failed to find faucet in bank with slot {0}")]
    FaucetNotFoundInBank(u64),

    #[error("Not enough lamports in faucet ({0}) to fund {1}")]
    NotEnoughLamportsInFaucetToFund(u64, u64),

    #[error("Crediting {0} to faucet which has {1} caused it to overflow")]
    FaucetOverflow(u64, u64),

    #[error("No banks forks available")]
    NoBankForksAvailable,

    #[error("Could not find executable data account '{0}' for program account '{1}'")]
    CouldNotFindExecutableDataAccount(Pubkey, Pubkey),

    #[error("The executable data of account '{1}' for program account '{1}' is does not hold program data")]
    InvalidExecutableDataAccountData(Pubkey, Pubkey),

    #[error("Invalid program data account '{0}' for program account '{1}'")]
    InvalidProgramDataContent(Pubkey, Pubkey),

    #[error("Not yet supporting cloning solana_loader_v4_program")]
    NotYetSupportingCloningSolanaLoader4Programs,

    #[error("Failed to clone executable data for '{0}' program ({1:?})")]
    FailedToCloneProgramExecutableDataAccount(
        Pubkey,
        solana_rpc_client_api::client_error::Error,
    ),
}
