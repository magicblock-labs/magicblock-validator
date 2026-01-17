use std::sync::Arc;

use async_trait::async_trait;
use magicblock_program::magic_scheduled_base_intent::{
    CommitType, CommittedAccount, MagicBaseIntent, ScheduledBaseIntent,
    UndelegateType,
};
use solana_account::Account;
use solana_pubkey::Pubkey;
use solana_signature::Signature;
use tracing::error;

use super::{CommitDiffTask, CommitTask};
use crate::{
    intent_executor::task_info_fetcher::{
        TaskInfoFetcher, TaskInfoFetcherError,
    },
    persist::IntentPersister,
    tasks::{
        args_task::{ArgsTask, ArgsTaskType},
        BaseActionTask, BaseTask, FinalizeTask, UndelegateTask,
    },
};

#[async_trait]
pub trait TasksBuilder {
    // Creates tasks for commit stage
    async fn commit_tasks<C: TaskInfoFetcher, P: IntentPersister>(
        commit_id_fetcher: &Arc<C>,
        base_intent: &ScheduledBaseIntent,
        persister: &Option<P>,
    ) -> TaskBuilderResult<Vec<Box<dyn BaseTask>>>;

    // Create tasks for finalize stage
    async fn finalize_tasks<C: TaskInfoFetcher>(
        info_fetcher: &Arc<C>,
        base_intent: &ScheduledBaseIntent,
    ) -> TaskBuilderResult<Vec<Box<dyn BaseTask>>>;
}

/// V1 Task builder
/// V1: Actions are part of finalize tx
pub struct TaskBuilderImpl;

// Accounts larger than COMMIT_STATE_SIZE_THRESHOLD use CommitDiff to
// reduce instruction size. Below this threshold, the commit is sent
// as CommitState. The value (256) is chosen because it is sufficient
// for small accounts, which typically could hold up to 8 u32 fields or
// 4 u64 fields. These integers are expected to be on the hot path
// and updated continuously.
pub const COMMIT_STATE_SIZE_THRESHOLD: usize = 256;

impl TaskBuilderImpl {
    pub fn create_commit_task(
        commit_id: u64,
        allow_undelegation: bool,
        account: CommittedAccount,
        base_account: Option<Account>,
    ) -> ArgsTask {
        let base_account =
            if account.account.data.len() > COMMIT_STATE_SIZE_THRESHOLD {
                base_account
            } else {
                None
            };

        if let Some(base_account) = base_account {
            ArgsTaskType::CommitDiff(CommitDiffTask {
                commit_id,
                allow_undelegation,
                committed_account: account,
                base_account,
            })
        } else {
            ArgsTaskType::Commit(CommitTask {
                commit_id,
                allow_undelegation,
                committed_account: account,
            })
        }
        .into()
    }
}

#[async_trait]
impl TasksBuilder for TaskBuilderImpl {
    /// Returns [`Task`]s for Commit stage
    async fn commit_tasks<C: TaskInfoFetcher, P: IntentPersister>(
        commit_id_fetcher: &Arc<C>,
        base_intent: &ScheduledBaseIntent,
        persister: &Option<P>,
    ) -> TaskBuilderResult<Vec<Box<dyn BaseTask>>> {
        let (accounts, allow_undelegation) = match &base_intent.base_intent {
            MagicBaseIntent::BaseActions(actions) => {
                let tasks = actions
                    .iter()
                    .map(|el| {
                        let task = BaseActionTask { action: el.clone() };
                        let task =
                            ArgsTask::new(ArgsTaskType::BaseAction(task));
                        Box::new(task) as Box<dyn BaseTask>
                    })
                    .collect();

                return Ok(tasks);
            }
            MagicBaseIntent::Commit(t) => (t.get_committed_accounts(), false),
            MagicBaseIntent::CommitAndUndelegate(t) => {
                (t.commit_action.get_committed_accounts(), true)
            }
        };

        let (commit_ids, base_accounts) = {
            let mut min_context_slot = 0;
            let committed_pubkeys = accounts
                .iter()
                .map(|account| {
                    min_context_slot =
                        std::cmp::max(min_context_slot, account.remote_slot);
                    account.pubkey
                })
                .collect::<Vec<_>>();

            let diffable_pubkeys = accounts
                .iter()
                .filter(|account| {
                    account.account.data.len() > COMMIT_STATE_SIZE_THRESHOLD
                })
                .map(|account| account.pubkey)
                .collect::<Vec<_>>();

            tokio::join!(
                commit_id_fetcher.fetch_next_commit_ids(
                    &committed_pubkeys,
                    min_context_slot
                ),
                commit_id_fetcher.get_base_accounts(
                    diffable_pubkeys.as_slice(),
                    min_context_slot
                )
            )
        };

        let commit_ids =
            commit_ids.map_err(TaskBuilderError::CommitTasksBuildError)?;

        let base_accounts = match base_accounts {
            Ok(map) => map,
            Err(err) => {
                tracing::warn!(intent_id = base_intent.id, error = ?err, "Failed to fetch base accounts, falling back to CommitState");
                Default::default()
            }
        };

        // Persist commit ids for commitees
        commit_ids
            .iter()
            .for_each(|(pubkey, commit_id) | {
                if let Err(err) = persister.set_commit_id(base_intent.id, pubkey, *commit_id) {
                    error!(intent_id = base_intent.id, pubkey = %pubkey, error = ?err, "Failed to persist commit id");
                }
            });

        let tasks = accounts
            .iter()
            .map(|account| {
                let commit_id = *commit_ids.get(&account.pubkey).expect("CommitIdFetcher provide commit ids for all listed pubkeys, or errors!");
                // TODO (snawaz): if accounts do not have duplicate, then we can use remove
                // instead:
                //  let base_account = base_accounts.remove(&account.pubkey);
                let base_account = base_accounts.get(&account.pubkey).cloned();
                let task = Self::create_commit_task(commit_id, allow_undelegation, account.clone(), base_account);
                Box::new(task) as Box<dyn BaseTask>
            }).collect();

        Ok(tasks)
    }

    /// Returns [`Task`]s for Finalize stage
    async fn finalize_tasks<C: TaskInfoFetcher>(
        info_fetcher: &Arc<C>,
        base_intent: &ScheduledBaseIntent,
    ) -> TaskBuilderResult<Vec<Box<dyn BaseTask>>> {
        // Helper to create a finalize task
        fn finalize_task(account: &CommittedAccount) -> Box<dyn BaseTask> {
            let task_type = ArgsTaskType::Finalize(FinalizeTask {
                delegated_account: account.pubkey,
            });
            Box::new(ArgsTask::new(task_type))
        }

        // Helper to create an undelegate task
        fn undelegate_task(
            account: &CommittedAccount,
            rent_reimbursement: &Pubkey,
        ) -> Box<dyn BaseTask> {
            let task_type = ArgsTaskType::Undelegate(UndelegateTask {
                delegated_account: account.pubkey,
                owner_program: account.account.owner,
                rent_reimbursement: *rent_reimbursement,
            });
            Box::new(ArgsTask::new(task_type))
        }

        // Helper to process commit types
        fn process_commit(commit: &CommitType) -> Vec<Box<dyn BaseTask>> {
            match commit {
                CommitType::Standalone(accounts) => {
                    accounts.iter().map(finalize_task).collect()
                }
                CommitType::WithBaseActions {
                    committed_accounts,
                    base_actions,
                } => {
                    let mut tasks = committed_accounts
                        .iter()
                        .map(finalize_task)
                        .collect::<Vec<_>>();
                    tasks.extend(base_actions.iter().map(|action| {
                        let task = BaseActionTask {
                            action: action.clone(),
                        };
                        let task =
                            ArgsTask::new(ArgsTaskType::BaseAction(task));
                        Box::new(task) as Box<dyn BaseTask>
                    }));
                    tasks
                }
            }
        }

        match &base_intent.base_intent {
            MagicBaseIntent::BaseActions(_) => Ok(vec![]),
            MagicBaseIntent::Commit(commit) => Ok(process_commit(commit)),
            MagicBaseIntent::CommitAndUndelegate(t) => {
                let mut tasks = process_commit(&t.commit_action);

                // Get rent reimbursments for undelegated accounts
                let accounts = t.get_committed_accounts();
                let mut min_context_slot = 0;
                let pubkeys = accounts
                    .iter()
                    .map(|account| {
                        min_context_slot = std::cmp::max(
                            min_context_slot,
                            account.remote_slot,
                        );
                        account.pubkey
                    })
                    .collect::<Vec<_>>();
                let rent_reimbursements = info_fetcher
                    .fetch_rent_reimbursements(&pubkeys, min_context_slot)
                    .await
                    .map_err(TaskBuilderError::FinalizedTasksBuildError)?;

                tasks.extend(accounts.iter().zip(rent_reimbursements).map(
                    |(account, rent_reimbursement)| {
                        undelegate_task(account, &rent_reimbursement)
                    },
                ));

                match &t.undelegate_action {
                    UndelegateType::Standalone => Ok(tasks),
                    UndelegateType::WithBaseActions(actions) => {
                        tasks.extend(actions.iter().map(|action| {
                            let task = BaseActionTask {
                                action: action.clone(),
                            };
                            let task =
                                ArgsTask::new(ArgsTaskType::BaseAction(task));
                            Box::new(task) as Box<dyn BaseTask>
                        }));

                        Ok(tasks)
                    }
                }
            }
        }
    }
}

#[derive(thiserror::Error, Debug)]
pub enum TaskBuilderError {
    #[error("CommitIdFetchError: {0}")]
    CommitTasksBuildError(#[source] TaskInfoFetcherError),
    #[error("FinalizedTasksBuildError: {0}")]
    FinalizedTasksBuildError(#[source] TaskInfoFetcherError),
}

impl TaskBuilderError {
    pub fn signature(&self) -> Option<Signature> {
        match self {
            Self::CommitTasksBuildError(err) => err.signature(),
            Self::FinalizedTasksBuildError(err) => err.signature(),
        }
    }
}

pub type TaskBuilderResult<T, E = TaskBuilderError> = Result<T, E>;
