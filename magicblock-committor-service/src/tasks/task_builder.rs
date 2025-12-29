use std::sync::Arc;

use async_trait::async_trait;
use log::error;
use magicblock_program::magic_scheduled_base_intent::{
    CommitType, CommittedAccount, MagicBaseIntent, ScheduledBaseIntent,
    UndelegateType,
};
use solana_pubkey::Pubkey;
use solana_signature::Signature;

use super::CommitTask;
use crate::{
    intent_executor::task_info_fetcher::{
        TaskInfoFetcher, TaskInfoFetcherError,
    },
    persist::IntentPersister,
    tasks::{BaseActionTask, FinalizeTask, Task, UndelegateTask},
};

#[async_trait]
pub trait TasksBuilder {
    // Creates tasks for commit stage
    async fn create_commit_tasks<C: TaskInfoFetcher, P: IntentPersister>(
        commit_id_fetcher: &Arc<C>,
        base_intent: &ScheduledBaseIntent,
        persister: &Option<P>,
    ) -> TaskBuilderResult<Vec<Task>>;

    // Create tasks for finalize stage
    async fn finalize_tasks<C: TaskInfoFetcher>(
        info_fetcher: &Arc<C>,
        base_intent: &ScheduledBaseIntent,
    ) -> TaskBuilderResult<Vec<Task>>;
}

/// V1 Task builder
/// V1: Actions are part of finalize tx
pub struct TaskBuilderImpl;

#[async_trait]
impl TasksBuilder for TaskBuilderImpl {
    /// Returns [`Task`]s for Commit stage
    async fn create_commit_tasks<C: TaskInfoFetcher, P: IntentPersister>(
        task_info_fetcher: &Arc<C>,
        base_intent: &ScheduledBaseIntent,
        persister: &Option<P>,
    ) -> TaskBuilderResult<Vec<Task>> {
        let (accounts, allow_undelegation) = match &base_intent.base_intent {
            MagicBaseIntent::BaseActions(actions) => {
                let tasks = actions
                    .iter()
                    .map(|el| {
                        Task::BaseAction(BaseActionTask { action: el.clone() })
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
            let committed_pubkeys = accounts
                .iter()
                .map(|account| account.pubkey)
                .collect::<Vec<_>>();

            let diffable_pubkeys = accounts
                .iter()
                .filter(|account| {
                    account.account.data.len()
                        > CommitTask::COMMIT_STATE_SIZE_THRESHOLD
                })
                .map(|account| account.pubkey)
                .collect::<Vec<_>>();

            tokio::join!(
                task_info_fetcher.fetch_next_commit_ids(&committed_pubkeys),
                task_info_fetcher
                    .get_base_accounts(diffable_pubkeys.as_slice())
            )
        };

        let commit_ids =
            commit_ids.map_err(TaskBuilderError::CommitTasksBuildError)?;

        let base_accounts = match base_accounts {
            Ok(map) => map,
            Err(err) => {
                log::warn!("Failed to fetch base accounts for CommitDiff (id={}): {}; falling back to CommitState", base_intent.id, err);
                Default::default()
            }
        };

        // Persist commit ids for commitees
        commit_ids
            .iter()
            .for_each(|(pubkey, commit_id) | {
                if let Err(err) = persister.set_commit_id(base_intent.id, pubkey, *commit_id) {
                    error!("Failed to persist commit id: {}, for message id: {} with pubkey {}: {}", commit_id, base_intent.id, pubkey, err);
                }
            });

        let tasks = accounts
            .iter()
            .map(|account| {
                let commit_id = *commit_ids.get(&account.pubkey).expect("CommitIdFetcher provide commit ids for all listed pubkeys, or errors!");
                            let base_account = base_accounts.get(&account.pubkey).cloned();

                Task::Commit(CommitTask::new(
                    commit_id,
                    allow_undelegation,
                    account.clone(),
                    base_account,
                ))
            }).collect();

        Ok(tasks)
    }

    /// Returns [`Task`]s for Finalize stage
    async fn finalize_tasks<C: TaskInfoFetcher>(
        info_fetcher: &Arc<C>,
        base_intent: &ScheduledBaseIntent,
    ) -> TaskBuilderResult<Vec<Task>> {
        // Helper to create a finalize task
        fn finalize_task(account: &CommittedAccount) -> Task {
            Task::Finalize(FinalizeTask {
                delegated_account: account.pubkey,
            })
        }

        // Helper to create an undelegate task
        fn undelegate_task(
            account: &CommittedAccount,
            rent_reimbursement: &Pubkey,
        ) -> Task {
            Task::Undelegate(UndelegateTask {
                delegated_account: account.pubkey,
                owner_program: account.account.owner,
                rent_reimbursement: *rent_reimbursement,
            })
        }

        // Helper to process commit types
        fn process_commit(commit: &CommitType) -> Vec<Task> {
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
                        Task::BaseAction(BaseActionTask {
                            action: action.clone(),
                        })
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
                let pubkeys = accounts
                    .iter()
                    .map(|account| account.pubkey)
                    .collect::<Vec<_>>();
                let rent_reimbursements = info_fetcher
                    .fetch_rent_reimbursements(&pubkeys)
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
                            Task::BaseAction(BaseActionTask {
                                action: action.clone(),
                            })
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
