use solana_sdk::hash::Hash;
use std::ops::Deref;

use magicblock_committor_program::{pdas, ChangedAccount, Changeset};
use solana_pubkey::Pubkey;
use solana_sdk::clock::Slot;

use crate::{
    error::{CommittorServiceError, CommittorServiceResult},
    persist::CommitType,
};

use super::previous_commit_state::PreviousCommitState;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RetryStep {
    CloseBufferAndChunksAccounts {
        /// The on chain address of the account we committed.
        pubkey: Pubkey,
        /// The ephemeral blockhash of the changeset we are writing,
        /// needed to properly derive the seeds of the PDAs.
        ephemeral_blockhash: Hash,
    },
    ProcessCommit {
        /// See [`crate::persist::CommitStatusRow::pubkey`]
        pubkey: Pubkey,
        /// See [`crate::persist::CommitStatusRow::delegated_account_owner`]
        delegated_account_owner: Pubkey,
        /// See [`crate::persist::CommitStatusRow::commit_type`]
        commit_type: CommitType,
        /// See [`crate::persist::CommitStatusRow::finalize`]
        finalize: bool,
        /// See [`crate::persist::CommitStatusRow::data`]
        data: Vec<u8>,
        /// See [`crate::persist::CommitStatusRow::lamports`]
        lamports: u64,
        /// See [`crate::persist::CommitStatusRow::slot`]
        slot: Slot,
        /// See [`crate::persist::CommitStatusRow::undelegate`]
        undelegate: bool,
    },
}

pub struct RetrySteps(Vec<RetryStep>);

impl Deref for RetrySteps {
    type Target = Vec<RetryStep>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl From<PreviousCommitState> for RetrySteps {
    fn from(state: PreviousCommitState) -> Self {
        let PreviousCommitState {
            pubkey,
            ephemeral_blockhash,
            ..
        } = state;

        let mut steps = vec![];
        if state.may_have_created_buffer_and_chunks_accounts() {
            steps.push(RetryStep::CloseBufferAndChunksAccounts {
                pubkey,
                ephemeral_blockhash,
            });
        }

        RetrySteps(steps)
    }
}

impl RetrySteps {
    pub fn cleanup_steps(&self) -> Vec<RetryStep> {
        self.iter()
            .filter(|step| {
                matches!(step, RetryStep::CloseBufferAndChunksAccounts { .. })
            })
            .cloned()
            .collect()
    }

    fn into_process_steps(self) -> Vec<RetryStep> {
        self.0
            .into_iter()
            .filter(|step| matches!(step, RetryStep::ProcessCommit { .. }))
            .collect()
    }

    pub fn try_into_changeset(
        self,
    ) -> CommittorServiceResult<(Changeset, bool)> {
        let steps = self.into_process_steps();
        if steps.is_empty() {
            return Err(
                CommittorServiceError::RetriedCommitsNeedAtLeastOneProcessCommitStep,
            );
        }
        // All accounts were originally committed as a single bundle, so we
        // just need to pick any ID as long it is the same for all
        const BUNDLE_ID: u64 = 1;

        let mut changeset = Changeset::default();
        let mut combined_finalize = None::<bool>;
        let mut combined_slot = None::<Slot>;
        for step in steps {
            if let RetryStep::ProcessCommit {
                pubkey,
                lamports,
                data,
                delegated_account_owner,
                undelegate,
                finalize,
                slot,
                ..
            } = step
            {
                let changed_account = ChangedAccount::Full {
                    lamports,
                    data,
                    owner: delegated_account_owner,
                    bundle_id: BUNDLE_ID,
                };
                changeset.add(pubkey, changed_account);
                if undelegate {
                    changeset.request_undelegation(pubkey);
                }
                match combined_finalize {
                    Some(x) if finalize != x => return Err(
                        CommittorServiceError::RetriedCommitsNeedToHaveSameFinalize,
                    ),
                    None => {
                        combined_finalize.replace(finalize);
                    }
                    _ => {}
                }

                match combined_slot {
                    Some(x) if slot != x => return Err(
                        CommittorServiceError::RetriedCommitsNeedToHaveSameSlot,
                    ),
                    None => {
                        combined_slot.replace(slot);
                    }
                    _ => {}
                }
            }
        }
        // SAFETY: we set the slot when processing first commit step
        changeset.slot = combined_slot.unwrap();
        // SAFETY: we set the finalize when processing first commit step
        let finalize = combined_finalize.unwrap();

        Ok((changeset, finalize))
    }
}
