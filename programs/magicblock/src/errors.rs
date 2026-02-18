use num_derive::{FromPrimitive, ToPrimitive};
use serde::Serialize;
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

    #[error("The account data for the provided key is missing.")]
    AccountDataMissing,

    #[error("Account already has a pending clone in progress")]
    CloneAlreadyPending,

    #[error("No pending clone found for account")]
    NoPendingClone,

    #[error("Clone offset mismatch")]
    CloneOffsetMismatch,

    #[error("The account is delegated and not currently undelegating.")]
    AccountIsDelegatedAndNotUndelegating,

    #[error(
        "Remote slot updates cannot be older than the current remote slot."
    )]
    IncomingRemoteSlotIsOlderThanCurrentRemoteSlot,
    #[error("Buffer account not found for program finalization")]
    BufferAccountNotFound,

    #[error("Failed to deploy program via LoaderV4")]
    ProgramDeployFailed,
}
