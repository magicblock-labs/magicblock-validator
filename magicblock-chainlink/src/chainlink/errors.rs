use solana_program::program_error::ProgramError;
use solana_pubkey::Pubkey;
use thiserror::Error;

use crate::remote_account_provider::RemoteAccountProviderError;

pub type ChainlinkResult<T> = std::result::Result<T, ChainlinkError>;

#[derive(Debug, Error)]
pub enum ChainlinkError {
    #[error("Remote account provider error: {0}")]
    RemoteAccountProviderError(
        #[from] crate::remote_account_provider::RemoteAccountProviderError,
    ),
    #[error("JoinError: {0}")]
    JoinError(#[from] tokio::task::JoinError),

    #[error("Cloner error: {0}")]
    ClonerError(#[from] crate::cloner::errors::ClonerError),

    #[error("Delegation record could not be decoded: {0} ({1:?})")]
    InvalidDelegationRecord(Pubkey, ProgramError),

    #[error("Failed to resolve one or more accounts {0} when getting delegation records")]
    DelegatedAccountResolutionsFailed(String),

    #[error("Failed to find account that was just resolved {0}")]
    ResolvedAccountCouldNoLongerBeFound(Pubkey),

    #[error("Failed to find companion account that was just resolved {0}")]
    ResolvedCompanionAccountCouldNoLongerBeFound(Pubkey),

    #[error("Failed to subscribe to account {0}: {1:?}")]
    FailedToSubscribeToAccount(Pubkey, RemoteAccountProviderError),

    #[error("Failed to resolve program data account {0} for program {1}")]
    FailedToResolveProgramDataAccount(Pubkey, Pubkey),

    #[error("Failed to resolve/deserialize one or more accounts {0} when getting programs")]
    ProgramAccountResolutionsFailed(String),
}
