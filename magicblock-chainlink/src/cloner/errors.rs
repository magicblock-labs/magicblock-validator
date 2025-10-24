use solana_pubkey::Pubkey;
use thiserror::Error;

pub type ClonerResult<T> = std::result::Result<T, ClonerError>;

#[derive(Debug, Error)]
pub enum ClonerError {
    #[error(transparent)]
    BincodeError(#[from] bincode::Error),
    #[error(transparent)]
    TryFromIntError(#[from] std::num::TryFromIntError),
    #[error(transparent)]
    TransactionError(#[from] solana_transaction_error::TransactionError),
    #[error(transparent)]
    RemoteAccountProviderError(
        #[from] crate::remote_account_provider::RemoteAccountProviderError,
    ),
    #[error("CommittorServiceError {0}")]
    CommittorServiceError(String),

    #[error("Failed to clone regular account {0} : {1:?}")]
    FailedToCloneRegularAccount(Pubkey, Box<ClonerError>),

    #[error("Failed to create clone program transaction {0} : {1:?}")]
    FailedToCreateCloneProgramTransaction(Pubkey, Box<ClonerError>),

    #[error("Failed to clone program {0} : {1:?}")]
    FailedToCloneProgram(Pubkey, Box<ClonerError>),
}
