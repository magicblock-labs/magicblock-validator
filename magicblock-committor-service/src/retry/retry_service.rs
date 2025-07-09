use std::{collections::HashMap, sync::Arc, time::Duration};

use log::*;
use magicblock_committor_program::Changeset;
use solana_pubkey::Pubkey;
use solana_sdk::hash::Hash;
use tokio::task::JoinSet;

use super::{
    previous_commit_state::PreviousCommitState,
    retry_steps::{RetryChangeset, RetryStep, RetrySteps},
};
use crate::{
    error::{CommittorServiceError, CommittorServiceResult},
    transactions::close_buffers_ix,
    ChangesetCommittor,
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
pub enum RetryKind {
    /// Processed and then finalized + undelegated accounts if so desired
    Process(usize),
    /// Only finalized + undelegated accounts if so desired
    Finalize(usize),
    /// Only undelegated accounts
    Undelegate(usize),
}

#[derive(Debug, PartialEq, Eq)]
pub struct RetryPendingResult {
    /// Amount of completed commits that were removed from the database for each reqid
    pub completed: HashMap<String, usize>,
    /// Amount of commits that were retried for each reqid
    pub retried: HashMap<String, RetryKind>,
}

impl<CC: ChangesetCommittor> CommittorRetryService<CC> {
    pub fn new(
        committor: Arc<CC>,
        config: CommittorRetryServiceConfig,
    ) -> Self {
        Self { committor, config }
    }

    /// Queries all commit statuses from the database and does the following:
    ///
    /// - removes all completed commit statuses
    /// - retries all failed commits and increases their retry count
    /// - ignores all pending commits
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
        let mut retries_by_reqid = HashMap::<String, RetryKind>::new();
        for (reqid, statuses) in failed.into_iter() {
            let statuses_len = statuses.len();
            let mut merged_changeset = None::<Changeset>;
            let mut combined_finalize = None::<bool>;
            let mut combined_ephemeral_blockhash = None::<Hash>;
            for status in statuses.into_iter() {
                let previous_commit =
                    PreviousCommitState::from((status, self.config.authority));
                let retry_steps = RetrySteps::from(previous_commit);

                // 1. Collect all cleanup steps
                let cleanup_steps = retry_steps.cleanup_steps();
                all_cleanup_steps.extend(cleanup_steps);

                // 2. Collect all process commit steps into a merged changeset
                let Some(RetryChangeset {
                    changeset,
                    finalize,
                    ephemeral_blockhash,
                }) = retry_steps.try_into_changeset()?
                else {
                    // TODO: @@@ we'll need to handle finalize/undelegate only as well
                    debug!(
                        "No changeset to process during retry for reqid: {}",
                        reqid
                    );
                    continue;
                };

                retries_by_reqid
                    .insert(reqid.clone(), RetryKind::Process(statuses_len));

                if let Some(ref mut cs) = merged_changeset {
                    cs.try_merge_with(changeset)?;
                } else {
                    merged_changeset.replace(changeset);
                }
                match combined_finalize {
                    Some(cf) if finalize != cf => {
                        return Err(
                            CommittorServiceError::RetriedCommitsOfSameRequestNeedConsistentFinalize(reqid),
                        );
                    }
                    None => {
                        combined_finalize.replace(finalize);
                    }
                    _ => {}
                }
                match combined_ephemeral_blockhash {
                    Some(cebh) if ephemeral_blockhash != cebh => {
                        return Err(
                            CommittorServiceError::RetriedCommitsNeedToHaveSameEphemeralBlockhash,
                        );
                    }
                    None => {
                        combined_ephemeral_blockhash
                            .replace(ephemeral_blockhash);
                    }
                    _ => {}
                }
            }

            if let Some(changeset) = merged_changeset {
                changesets.insert(
                    reqid,
                    RetryChangeset {
                        changeset,
                        // SAFETY: combined_finalize is set at first loop iteration
                        finalize: combined_finalize.unwrap(),
                        // SAFETY: combined_ephemeral_blockhash is set at first loop iteration
                        ephemeral_blockhash: combined_ephemeral_blockhash
                            .unwrap(),
                    },
                );
            }
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
                    // TODO: @@@
                    todo!("BUG: Logic error")
                }
            }
        }
        self.committor
            .run_validator_signed_ixs(cleanup_ixs)
            .await??;

        // Recommit changesets
        let mut join_set = JoinSet::new();
        for (
            reqid,
            RetryChangeset {
                changeset,
                finalize,
                ephemeral_blockhash,
            },
        ) in changesets.into_iter()
        {
            join_set.spawn(self.committor.recommit_changeset(
                reqid.clone(),
                changeset,
                ephemeral_blockhash,
                finalize,
            ));
        }
        join_set.join_all().await;

        Ok(RetryPendingResult {
            completed,
            retried: retries_by_reqid,
        })
    }
}
