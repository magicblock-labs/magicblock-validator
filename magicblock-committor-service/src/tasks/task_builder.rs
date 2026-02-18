use std::{collections::HashMap, sync::Arc};

use async_trait::async_trait;
use magicblock_program::magic_scheduled_base_intent::{
    BaseAction, CommitType, CommittedAccount, ScheduledIntentBundle,
    UndelegateType,
};
use solana_account::Account;
use solana_pubkey::Pubkey;
use solana_signature::Signature;
use tracing::error;

use crate::{
    intent_executor::task_info_fetcher::{
        TaskInfoFetcher, TaskInfoFetcherError, TaskInfoFetcherResult,
    },
    persist::IntentPersister,
    tasks::{
        commit_task::{CommitDeliveryDetails, CommitTask},
        BaseActionTask, BaseTaskImpl, FinalizeTask, UndelegateTask,
    },
};

#[async_trait]
pub trait TasksBuilder {
    // Creates tasks for commit stage
    async fn commit_tasks<C: TaskInfoFetcher, P: IntentPersister>(
        commit_id_fetcher: &Arc<C>,
        base_intent: &ScheduledIntentBundle,
        persister: &Option<P>,
    ) -> TaskBuilderResult<Vec<BaseTaskImpl>>;

    // Create tasks for finalize stage
    async fn finalize_tasks<C: TaskInfoFetcher>(
        info_fetcher: &Arc<C>,
        base_intent: &ScheduledIntentBundle,
    ) -> TaskBuilderResult<Vec<BaseTaskImpl>>;
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
    ) -> CommitTask {
        let base_account =
            if account.account.data.len() > COMMIT_STATE_SIZE_THRESHOLD {
                base_account
            } else {
                None
            };

        let delivery_details = if let Some(base_account) = base_account {
            CommitDeliveryDetails::DiffInArgs { base_account }
        } else {
            CommitDeliveryDetails::StateInArgs
        };

        CommitTask {
            commit_id,
            allow_undelegation,
            committed_account: account,
            delivery_details,
        }
    }

    fn create_action_tasks(actions: &[BaseAction]) -> Vec<BaseTaskImpl> {
        actions
            .iter()
            .map(|el| BaseActionTask { action: el.clone() }.into())
            .collect()
    }

    async fn fetch_commit_nonces<C: TaskInfoFetcher>(
        task_info_fetcher: &Arc<C>,
        accounts: &[(bool, CommittedAccount)],
        min_context_slot: u64,
    ) -> TaskInfoFetcherResult<HashMap<Pubkey, u64>> {
        let committed_pubkeys = accounts
            .iter()
            .map(|(_, account)| account.pubkey)
            .collect::<Vec<_>>();

        task_info_fetcher
            .fetch_next_commit_ids(&committed_pubkeys, min_context_slot)
            .await
    }

    async fn fetch_diffable_accounts<C: TaskInfoFetcher>(
        task_info_fetcher: &Arc<C>,
        accounts: &[(bool, CommittedAccount)],
        min_context_slot: u64,
    ) -> TaskInfoFetcherResult<HashMap<Pubkey, Account>> {
        let diffable_pubkeys = accounts
            .iter()
            .filter(|(_, account)| {
                account.account.data.len() > COMMIT_STATE_SIZE_THRESHOLD
            })
            .map(|(_, account)| account.pubkey)
            .collect::<Vec<_>>();

        task_info_fetcher
            .get_base_accounts(&diffable_pubkeys, min_context_slot)
            .await
    }
}

#[async_trait]
impl TasksBuilder for TaskBuilderImpl {
    /// Returns [`Task`]s for Commit stage
    async fn commit_tasks<C: TaskInfoFetcher, P: IntentPersister>(
        task_info_fetcher: &Arc<C>,
        intent_bundle: &ScheduledIntentBundle,
        persister: &Option<P>,
    ) -> TaskBuilderResult<Vec<BaseTaskImpl>> {
        let mut tasks = Vec::new();
        tasks.extend(Self::create_action_tasks(
            intent_bundle.standalone_actions().as_slice(),
        ));

        let committed_accounts =
            intent_bundle.get_commit_intent_accounts().cloned();
        let undelegated_accounts =
            intent_bundle.get_undelegate_intent_accounts().cloned();
        let flagged_accounts: Vec<_> =
            [(false, committed_accounts), (true, undelegated_accounts)]
                .into_iter()
                .flat_map(|(allow_undelegation, accounts)| {
                    accounts
                        .into_iter()
                        .flatten()
                        .map(move |account| (allow_undelegation, account))
                })
                .collect();

        // Get commit nonces and base accounts
        let min_context_slot = flagged_accounts
            .iter()
            .map(|(_, account)| account.remote_slot)
            .max()
            .unwrap_or(0);
        let (commit_ids, base_accounts) = tokio::join!(
            Self::fetch_commit_nonces(
                task_info_fetcher,
                &flagged_accounts,
                min_context_slot
            ),
            Self::fetch_diffable_accounts(
                task_info_fetcher,
                &flagged_accounts,
                min_context_slot
            )
        );
        let commit_ids =
            commit_ids.map_err(TaskBuilderError::CommitTasksBuildError)?;
        let mut base_accounts = base_accounts.unwrap_or_else(|err| {
            tracing::warn!(intent_id = intent_bundle.id, error = ?err, "Failed to fetch base accounts, falling back to CommitState");
            Default::default()
        });

        // Persist commit ids for commitees
        commit_ids
            .iter()
            .for_each(|(pubkey, commit_id) | {
                if let Err(err) = persister.set_commit_id(intent_bundle.id, pubkey, *commit_id) {
                    error!(intent_id = intent_bundle.id, pubkey = %pubkey, error = ?err, "Failed to persist commit id");
                }
            });

        // Create commit tasks
        let commit_tasks_iter = flagged_accounts.into_iter().map(
            |(allow_undelegation, account)| {
                let commit_id = commit_ids
                    .get(&account.pubkey)
                    .copied()
                    .unwrap_or_else(|| {
                        // This shall not ever happen since TaskInfoFetcher
                        // returns commit ids for all pubkeys or throws
                        // If it does occur, it will be patched and retried by IntentExecutor
                        error!(pubkey = %account.pubkey, "Commit id absent for pubkey");
                        0
                    });
                let base_account = base_accounts.remove(&account.pubkey);

                Self::create_commit_task(
                    commit_id,
                    allow_undelegation,
                    account.clone(),
                    base_account,
                )
                .into()
            },
        );
        tasks.extend(commit_tasks_iter);

        Ok(tasks)
    }

    /// Returns [`Task`]s for Finalize stage
    async fn finalize_tasks<C: TaskInfoFetcher>(
        info_fetcher: &Arc<C>,
        intent_bundle: &ScheduledIntentBundle,
    ) -> TaskBuilderResult<Vec<BaseTaskImpl>> {
        // Helper to create a finalize task
        fn finalize_task(account: &CommittedAccount) -> BaseTaskImpl {
            FinalizeTask {
                delegated_account: account.pubkey,
            }
            .into()
        }

        // Helper to create an undelegate task
        fn undelegate_task(
            account: &CommittedAccount,
            rent_reimbursement: &Pubkey,
        ) -> BaseTaskImpl {
            UndelegateTask {
                delegated_account: account.pubkey,
                owner_program: account.account.owner,
                rent_reimbursement: *rent_reimbursement,
            }
            .into()
        }

        // Helper to process commit types
        fn process_commit(commit: &CommitType) -> Vec<BaseTaskImpl> {
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
                        let task: BaseTaskImpl = BaseActionTask {
                            action: action.clone(),
                        }
                        .into();
                        task
                    }));
                    tasks
                }
            }
        }

        let mut tasks = Vec::new();
        if let Some(ref value) = intent_bundle.intent_bundle.commit {
            tasks.extend(process_commit(value));
        }

        if let Some(ref value) =
            intent_bundle.intent_bundle.commit_and_undelegate
        {
            tasks.extend(process_commit(&value.commit_action));

            // Get rent reimbursments for undelegated accounts
            let accounts = value.get_committed_accounts();
            let mut min_context_slot = 0;
            let pubkeys = accounts
                .iter()
                .map(|account| {
                    min_context_slot =
                        std::cmp::max(min_context_slot, account.remote_slot);
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

            if let UndelegateType::WithBaseActions(actions) =
                &value.undelegate_action
            {
                tasks.extend(actions.iter().map(|action| {
                    let task: BaseTaskImpl = BaseActionTask {
                        action: action.clone(),
                    }
                    .into();
                    task
                }));
            }
        };

        Ok(tasks)
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
