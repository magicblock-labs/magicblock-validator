use std::{collections::HashMap, sync::Arc, time::Duration};

use magicblock_committor_program::Changeset;
use solana_pubkey::Pubkey;

use crate::{
    error::{CommittorServiceError, CommittorServiceResult},
    persist::CommitStatusRow,
    transactions::close_buffers_ix,
    ChangesetCommittor,
};

use super::{
    previous_commit_state::PreviousCommitState,
    retry_steps::{RetryStep, RetrySteps},
};

#[derive(Default)]
pub struct CommittorRetryServiceConfig {
    /// The authority to use when retrying the commits. This needs to be the same
    /// as the authority used to create the original commit.
    pub authority: Pubkey,
    /// The frequency at which the retry service will check for commits that need
    /// to be retried.
    /// If `None`, the service will not retry commits automatically and is triggered
    /// manually instead.
    pub frequency: Option<Duration>,
}

pub struct CommittorRetryService<CC: ChangesetCommittor> {
    committor: Arc<CC>,
    config: CommittorRetryServiceConfig,
}

#[derive(Debug, PartialEq, Eq)]
pub struct RetryPendingResult {
    pub completed: HashMap<String, usize>,
    pub failed: Vec<(String, Vec<CommitStatusRow>)>,
}

impl<CC: ChangesetCommittor> CommittorRetryService<CC> {
    pub fn new(
        committor: Arc<CC>,
        config: CommittorRetryServiceConfig,
    ) -> Self {
        Self { committor, config }
    }

    pub async fn retry_failed(
        &self,
    ) -> CommittorServiceResult<RetryPendingResult> {
        let reqids_res = self.committor.get_reqids().await?;
        let reqids = reqids_res?;

        let mut statuses_by_reqid = HashMap::new();
        for reqid in reqids.into_iter() {
            let statuses =
                self.committor.get_commit_statuses(reqid.clone()).await??;
            statuses_by_reqid.insert(reqid, statuses);
        }

        let (complete, failed) = statuses_by_reqid.into_iter().fold(
            (vec![], vec![]),
            |(mut complete, mut failed), (reqid, statuses)| {
                if statuses.iter().all(|s| s.commit_status.is_complete()) {
                    complete.push(reqid);
                } else if statuses.iter().any(|s| s.commit_status.has_failed())
                {
                    failed.push((reqid, statuses));
                }
                // NOTE: we are ignoring pending commits
                (complete, failed)
            },
        );

        // Remove all completed entries
        let mut completed = HashMap::new();
        for reqid in complete.into_iter() {
            let removed = self
                .committor
                .remove_commit_statuses_with_reqid(reqid.clone())
                .await??;
            completed.insert(reqid, removed);
        }

        // Prepare the changesets as well as the cleanup steps we need to run
        // before recommitting them
        let mut all_cleanup_steps = vec![];
        let mut changesets = HashMap::new();
        for (reqid, statuses) in failed.into_iter() {
            let mut merged_changeset = None::<Changeset>;
            let mut combined_finalize = None::<bool>;
            for status in statuses.into_iter() {
                let previous_commit = PreviousCommitState::from((
                    status,
                    self.config.authority.clone(),
                ));
                let retry_steps = RetrySteps::from(previous_commit);
                let cleanup_steps = retry_steps.cleanup_steps();
                all_cleanup_steps.extend(cleanup_steps);

                let (changeset, finalize) = retry_steps.try_into_changeset()?;
                if let Some(ref mut cs) = merged_changeset {
                    cs.try_merge_with(changeset)?;
                } else {
                    merged_changeset.replace(changeset);
                }
                if let Some(cf) = combined_finalize {
                    if finalize != cf {
                        return Err(
                            CommittorServiceError::RetriedCommitsNeedConsistentFinalize(reqid),
                        );
                    }
                } else {
                    combined_finalize.replace(finalize);
                }
            }
            changesets.insert(
                reqid,
                (merged_changeset.ok_or_else(|| {
                    CommittorServiceError::RetriedCommitsNeedAtLeastOneProcessCommitStep
                })?, combined_finalize.unwrap_or(false)),
            );
        }

        // Run the cleanup steps
        let mut cleanup_ixs = vec![];
        for step in all_cleanup_steps.into_iter() {
            use RetryStep::*;
            match step {
                CloseBufferAndChunksAccounts {
                    pubkey,
                    ephemeral_blockhash,
                } => {
                    let ix = close_buffers_ix(
                        self.config.authority,
                        &pubkey,
                        &ephemeral_blockhash,
                    );
                    cleanup_ixs.push(ix);
                }
                ProcessCommit { .. } => {
                    todo!("BUG: Logic error")
                }
            }
        }
        self.committor
            .run_validator_signed_ixs(cleanup_ixs)
            .await??;

        // Recommit changesets

        Ok(RetryPendingResult { completed, failed })
    }
}
