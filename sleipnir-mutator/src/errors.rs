use thiserror::Error;

pub type MutatorResult<T> = std::result::Result<T, MutatorError>;

#[derive(Error, Debug)]
pub enum MutatorError {
    #[error("ParsePubkeyError: '{0}' ({0:?})")]
    ParsePubkeyError(#[from] solana_sdk::pubkey::ParsePubkeyError),

    // #[error("RpcClientError: '{0}' ({0:?})")]
    // RpcClientError(#[from] sleipnir_rpc_client_api::client_error::Error),
    #[error("StdError: '{0}' ({0:?})")]
    StdError(#[from] Box<dyn std::error::Error>),

    #[error("Invalid cluster '{0}'")]
    InvalidCluster(String),

    #[error("Bank forks not set")]
    BankForksNotSet,

    #[error("Failed to modify account '{0}' ({1})")]
    FailedToModifyAccount(String, String),

    #[error("Failed to clone account '{0}' ({1})")]
    FailedToCloneAccount(String, String),

    #[error("Failed to get lamports of development account '{0}' ({1})")]
    FailedToGetLamportsOfDevelopmentAccount(String, String),

    #[error("Failed to find faucet in bank with slot {0}")]
    FaucetNotFoundInBank(u64),

    #[error("Not enough lamports in faucet ({0}) to fund {1}")]
    NotEnoughLamportsInFaucetToFund(u64, u64),

    #[error("Crediting {0} to faucet which has {1} caused it to overflow")]
    FaucetOverflow(u64, u64),

    #[error("No banks forks available")]
    NoBankForksAvailable,

    #[error("Could not find executable data account '{0}' for program account '{1}'")]
    CouldNotFindExecutableDataAccount(String, String),
}
