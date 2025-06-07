use solana_pubkey::Pubkey;
use solana_sdk::hash::Hash;

use crate::persist::{CommitStatus, CommitType};

pub struct PreviousCommitState {
    /// See [`crate::persist::CommitStatusRow::pubkey`]
    pub pubkey: Pubkey,
    /// See [`crate::persist::CommitStatusRow::delegated_account_owner`]
    pub delegated_account_owner: Pubkey,
    /// See [`magicblock_committor_program::instruction::CreateInitIxArgs::authority`]
    pub authority: Pubkey,
    /// See [`crate::persist::CommitStatusRow::ephemeral_blockhash`]
    pub ephemeral_blockhash: Hash,
    /// See [`crate::persist::CommitStatusRow::commit_status`]
    pub commit_status: CommitStatus,
    /// See [`crate::persist::CommitStatusRow::commit_type`]
    pub commit_type: CommitType,
    /// See [`crate::persist::CommitStatusRow::finalize`]
    pub finalize: bool,
    /// See [`crate::persist::CommitStatusRow::data`]
    pub data: Option<Vec<u8>>,
    /// See [`crate::persist::CommitStatusRow::lamports`]
    pub lamports: u64,
}

impl PreviousCommitState {
    pub(crate) fn created_buffer_and_chunks_accounts(&self) -> bool {
        use CommitStatus::*;
        matches!(
            self.commit_status,
            BufferAndChunkPartiallyInitialized(_)
                | BufferAndChunkInitialized(_)
                | BufferAndChunkFullyInitialized(_)
        )
    }

    /// Currently we don't record the committ strategy, thus we need to assume for
    /// all failures that would occur after buffer initialization that the buffers
    /// and chunks accounts were created
    /// Does not return `true` if the commits succeeded as in that case the
    /// buffer accounts were cleaned up as well.
    pub(crate) fn may_have_created_buffer_and_chunks_accounts(&self) -> bool {
        if self.created_buffer_and_chunks_accounts() {
            return true;
        }
        if self.commit_type == CommitType::EmptyAccount {
            return false;
        }
        use CommitStatus::*;
        matches!(
            self.commit_status,
            PartOfTooLargeBundleToProcess(_)
                | FailedProcess(_)
                | FailedFinalize(_)
                | FailedUndelegate(_)
        )
    }

    /// This returns `true` if the errors we encountered during the commit happened
    /// after it was processed, i.e. it couldn't be finalized or account undelegation
    /// failed.
    /// Also returns `true` if the commit was successful.
    pub(crate) fn succeeded_process(&self) -> bool {
        use CommitStatus::*;
        matches!(
            self.commit_status,
            FailedFinalize(_) | FailedUndelegate(_) | Succeeded(_)
        )
    }

    /// This returns `true` if the errors we encountered during the commit happened
    /// after it was finalized, i.e. account undelegation failed.
    /// Also returns `true` if the commit was successful.
    fn succeeded_finalize(&self) -> bool {
        use CommitStatus::*;
        matches!(self.commit_status, FailedUndelegate(_) | Succeeded(_))
    }

    /// This returns `true` if the entire commit was successful and nothing needs
    /// to be done. We can remove the entry from the database.
    fn succeeded(&self) -> bool {
        use CommitStatus::*;
        matches!(self.commit_status, Succeeded(_))
    }
}
