use solana_sdk::pubkey::Pubkey;
use thiserror::Error;

pub type MutatorResult<T> = Result<T, MutatorError>;

#[derive(Error, Debug)] // Note: This is not clonable unlike MutatorModificationError
pub enum MutatorError {
    #[error("RpcClientError: '{0}' ({0:?})")]
    RpcClientError(#[from] solana_rpc_client_api::client_error::Error),

    #[error(transparent)]
    PubkeyError(#[from] solana_sdk::pubkey::PubkeyError),

    #[error(transparent)]
    MutatorModificationError(#[from] MutatorModificationError),
}

pub type MutatorModificationResult<T> = Result<T, MutatorModificationError>;

#[derive(Debug, Clone, Error)]
pub enum MutatorModificationError {
    #[error("Could not find executable data account '{0}' for program account '{1}'")]
    CouldNotFindExecutableDataAccount(Pubkey, Pubkey),

    #[error("Invalid program data account '{0}' for program account '{1}'")]
    InvalidProgramDataContent(Pubkey, Pubkey),
}
