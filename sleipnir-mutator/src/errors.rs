use solana_sdk::pubkey::Pubkey;
use thiserror::Error;

pub type MutatorResult<T> = std::result::Result<T, MutatorError>;

#[derive(Error, Debug)]
pub enum MutatorError {
    #[error("RpcClientError: '{0}' ({0:?})")]
    RpcClientError(#[from] solana_rpc_client_api::client_error::Error),

    #[error(transparent)]
    PubkeyError(#[from] solana_sdk::pubkey::PubkeyError),

    #[error("Could not find executable data account '{0}' for program account '{1}'")]
    CouldNotFindExecutableDataAccount(Pubkey, Pubkey),

    #[error("Invalid program data account '{0}' for program account '{1}'")]
    InvalidProgramDataContent(Pubkey, Pubkey),

    #[error("Failed to clone executable data for '{0}' program ({1:?})")]
    FailedToCloneProgramExecutableDataAccount(
        Pubkey,
        solana_rpc_client_api::client_error::Error,
    ),
}
