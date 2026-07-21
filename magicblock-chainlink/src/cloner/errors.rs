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
    SignerError(#[from] solana_signer::SignerError),
    #[error(transparent)]
    TransactionError(#[from] solana_transaction_error::TransactionError),
    #[error(transparent)]
    RemoteAccountProviderError(
        #[from] crate::remote_account_provider::RemoteAccountProviderError,
    ),
    #[error("CommittorServiceError {0}")]
    CommittorServiceError(String),

    #[error("engine error: {0}")]
    Engine(String),

    /// The engine has no equivalent yet of the magic program's
    /// `ScheduleUndelegation`, which the cloner used to emit alongside the
    /// clone. Cloning without it would leave an account that must be
    /// undelegated (e.g. flagged for AML risk) delegated instead, so this
    /// refuses rather than silently skipping the undelegation. Scheduled to be
    /// wired back up in phase 4.
    #[error(
        "account {0} must be undelegated after cloning, which is not supported until the engine can schedule undelegations"
    )]
    UndelegationSchedulingUnavailable(Pubkey),

    #[error(
        "Clone transaction for account {pubkey} is too large: {size} bytes (max {max_size} bytes)"
    )]
    CloneTransactionTooLarge {
        pubkey: Pubkey,
        size: usize,
        max_size: usize,
    },

    #[error("Failed to clone regular account {0} : {1:?}")]
    FailedToCloneRegularAccount(Pubkey, Box<ClonerError>),

    #[error("Failed to create clone program transaction {0} : {1:?}")]
    FailedToCreateCloneProgramTransaction(Pubkey, Box<ClonerError>),

    #[error("Failed to clone program {0} : {1:?}")]
    FailedToCloneProgram(Pubkey, Box<ClonerError>),

    #[error(
        "Failed to clone and schedule undelegation for account {0} : {1:?}"
    )]
    FailedToCloneAndScheduleUndelegation(Pubkey, Box<ClonerError>),

    #[error("Failed to evict account {0} : {1:?}")]
    FailedToEvictAccount(Pubkey, Box<ClonerError>),

    #[error("Failed to schedule undelegation {0} : {1:?}")]
    FailedToScheduleUndelegation(Pubkey, Box<ClonerError>),
}
