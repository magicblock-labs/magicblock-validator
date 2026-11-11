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

#[derive(Error, Debug, Serialize, Clone, PartialEq, Eq, FromPrimitive, ToPrimitive)]
pub enum MagicBlockProgramError {
    #[error("need at least one account to modify")]
    NoAccountsToModify,

    #[error("number of accounts to modify needs to match number of account modifications")]
    AccountsToModifyNotMatchingAccountModifications,

    #[error("The account modification for the provided key is missing")]
    AccountModificationMissing,

    #[error("first account needs to be MagicBlock authority")]
    FirstAccountNeedsToBeMagicBlockAuthority,

    #[error("MagicBlock authority needs to be owned by system program")]
    MagicBlockAuthorityNeedsToBeOwnedBySystemProgram,

    #[error("The account data for the provided key is missing")]
    AccountDataMissing,

    #[error("Account already has a pending clone in progress")]
    CloneAlreadyPending,

    #[error("No pending clone found for account")]
    NoPendingClone,

    #[error("Clone offset mismatch")]
    CloneOffsetMismatch,

    #[error("The account is delegated and not currently undelegating")]
    AccountIsDelegated,

    /// Keep this placeholder in place so later program error codes remain stable.
    /// Replace it in this position when assigning a defined error to this slot.
    #[error("unused program error code")]
    Unused,

    #[error("Updates cannot be older than the current remote slot")]
    OutOfOrderUpdate,

    #[error("Buffer account not found for program finalization")]
    BufferAccountNotFound,

    #[error("Duplicate delegated-account cloning within same slot")]
    DuplicateDelegatedAccountClone,

    #[error("Delegated clone owner cannot be the delegation program")]
    DelegatedCloneOwnerIsDelegationProgram,

    #[error("Post-delegation actions require a delegated clone target")]
    PostDelegationActionsRequireDelegatedClone,

    #[error("Post-delegation actions cannot use validator authority as signer")]
    PostDelegationActionUsesValidatorAuthority,

    #[error("Post-delegation action executor is missing")]
    PostDelegationActionExecutorMissing,

    #[error("Post-delegation action executor does not match clone")]
    PostDelegationActionExecutorMismatch,

    #[error("Post-delegation action executor must be top-level")]
    PostDelegationActionExecutorNotTopLevel,

    #[error("Account modification instructions are no longer supported")]
    AccountCompositionRemoved,
}

#[cfg(test)]
mod tests {
    use solana_instruction::error::InstructionError;

    use super::MagicBlockProgramError;

    #[test]
    fn account_composition_removed_keeps_custom_error_code() {
        let error: InstructionError = MagicBlockProgramError::AccountCompositionRemoved.into();

        assert_eq!(error, InstructionError::Custom(20));
    }

    #[test]
    fn unused_error_slot_stays_at_code_ten() {
        let error: InstructionError = MagicBlockProgramError::Unused.into();

        assert_eq!(error, InstructionError::Custom(10));
    }
}
