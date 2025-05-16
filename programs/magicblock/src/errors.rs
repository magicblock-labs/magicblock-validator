use num_derive::{FromPrimitive, ToPrimitive};
use serde::Serialize;
use solana_sdk::decode_error::DecodeError;
use thiserror::Error;

// -----------------
// Program CustomError Codes
// -----------------
pub mod custom_error_codes {
    pub const FAILED_TO_TRANSFER_SCHEDULE_COMMIT_COST: u32 = 10_000;
    pub const UNABLE_TO_UNLOCK_SENT_COMMITS: u32 = 10_001;
    pub const CANNOT_FIND_SCHEDULED_COMMIT: u32 = 10_002;
}

#[derive(
    Error, Debug, Serialize, Clone, PartialEq, Eq, FromPrimitive, ToPrimitive,
)]
pub enum MagicBlockProgramError {
    #[error("need at least one account to modify")]
    NoAccountsToModify,

    #[error("number of accounts to modify needs to match number of account modifications")]
    AccountsToModifyNotMatchingAccountModifications,

    #[error("The account modification for the provided key is missing.")]
    AccountModificationMissing,

    #[error("first account needs to be MagicBlock authority")]
    FirstAccountNeedsToBeMagicBlockAuthority,

    #[error("MagicBlock authority needs to be owned by system program")]
    MagicBlockAuthorityNeedsToBeOwnedBySystemProgram,

    #[error("The account resolution for the provided key failed.")]
    AccountDataResolutionFailed,

    #[error("The account data for the provided key is missing both from in-memory and ledger storage.")]
    AccountDataMissing,

    #[error("The account data for the provided key is missing from in-memory and we are not replaying the ledger.")]
    AccountDataMissingFromMemory,

    #[error("Tried to persist data that could not be resolved.")]
    AttemptedToPersistUnresolvedData,

    #[error("Tried to persist data that was resolved from storage.")]
    AttemptedToPersistDataFromStorage,

    #[error("Encountered an error when persisting account modification data.")]
    FailedToPersistAccountModData,
}

impl<T> DecodeError<T> for MagicBlockProgramError {
    fn type_of() -> &'static str {
        "MagicBlockProgramError"
    }
}
