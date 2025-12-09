use thiserror::Error;

pub type CommitPersistResult<T> = Result<T, CommitPersistError>;

#[derive(Error, Debug)]
pub enum CommitPersistError {
    #[error("RusqliteError: '{0}' ({0:?})")]
    RusqliteError(#[from] rusqlite::Error),

    #[error("ParsePubkeyError: '{0}' ({0:?})")]
    ParsePubkeyError(#[from] solana_pubkey::ParsePubkeyError),

    #[error("ParseSignatureError: '{0}' ({0:?})")]
    ParseSignatureError(#[from] solana_signature::ParseSignatureError),

    #[error("ParseHashError: '{0}' ({0:?})")]
    ParseHashError(#[from] solana_hash::ParseHashError),

    #[error("Invalid Commit Type: '{0}' ({0:?})")]
    InvalidCommitType(String),

    #[error("Invalid Commit Status: '{0}' ({0:?})")]
    InvalidCommitStatus(String),

    #[error("Invalid Commit Strategy: '{0}' ({0:?})")]
    InvalidCommitStrategy(String),

    #[error(
        "Commit Status update requires status with bundle id: '{0}' ({0:?})"
    )]
    CommitStatusUpdateRequiresStatusWithBundleId(String),

    #[error("Commit Status needs bundle id: '{0}' ({0:?})")]
    CommitStatusNeedsBundleId(String),

    #[error("Commit Status needs signatures: '{0}' ({0:?})")]
    CommitStatusNeedsSignatures(String),

    #[error("Commit Status needs commit strategy: '{0}' ({0:?})")]
    CommitStatusNeedsStrategy(String),
}
