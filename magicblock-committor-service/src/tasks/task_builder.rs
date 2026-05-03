use std::{collections::HashMap, sync::Arc};

use async_trait::async_trait;
use futures_util::future::try_join_all;
use magicblock_core::intent::CommittedAccount;
use magicblock_program::magic_scheduled_base_intent::{
    BaseAction, CommitAndUndelegate, CommitType, ScheduledIntentBundle,
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
        commit_task::{CommitDelivery, CommitTask},
        BaseActionTask, BaseActionTaskV1, BaseActionTaskV2, BaseTaskImpl,
        CommitFinalizeTask, FinalizeTask, UndelegateTask,
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

/// Builds base-layer tasks for scheduled intents.
/// In [`CommitExecutionMode::SeparateCommitAndFinalize`], commit actions stay in
/// the finalize stage. In [`CommitExecutionMode::CommitFinalize`], they run
/// after the combined commit-finalize task.
pub struct TaskBuilderImpl;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommitExecutionMode {
    ///
    /// Execute CommitFinalize as single instruction
    ///
    CommitFinalize,

    ///
    /// Execute Commit and Finalize as two separate instructions
    ///
    SeparateCommitAndFinalize,
}

pub const DEFAULT_COMMIT_EXECUTION_MODE: CommitExecutionMode =
    CommitExecutionMode::CommitFinalize;

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
            CommitDelivery::DiffInArgs { base_account }
        } else {
            CommitDelivery::StateInArgs
        };

        CommitTask {
            commit_id,
            allow_undelegation,
            committed_account: account,
            delivery_details,
        }
    }

    fn create_action_tasks<'a>(
        actions: &'a [BaseAction],
    ) -> impl Iterator<Item = BaseTaskImpl> + 'a {
        actions.iter().map(|action| {
            let task = match action.source_program {
                Some(source_program) => BaseActionTask::V2(BaseActionTaskV2 {
                    action: action.clone(),
                    source_program,
                }),
                None => BaseActionTask::V1(BaseActionTaskV1 {
                    action: action.clone(),
                }),
            };
            task.into()
        })
    }

    async fn fetch_commit_nonces<C: TaskInfoFetcher>(
        task_info_fetcher: &Arc<C>,
        accounts: &[CommittedAccount],
        min_context_slot: u64,
    ) -> TaskInfoFetcherResult<HashMap<Pubkey, u64>> {
        let committed_pubkeys = accounts
            .iter()
            .map(|account| account.pubkey)
            .collect::<Vec<_>>();

        task_info_fetcher
            .fetch_next_commit_nonces(&committed_pubkeys, min_context_slot)
            .await
    }

    async fn fetch_diffable_accounts<C: TaskInfoFetcher>(
        task_info_fetcher: &Arc<C>,
        accounts: &[CommittedAccount],
        min_context_slot: u64,
    ) -> TaskInfoFetcherResult<HashMap<Pubkey, Account>> {
        let diffable_pubkeys = accounts
            .iter()
            .filter(|account| {
                account.account.data.len() > COMMIT_STATE_SIZE_THRESHOLD
            })
            .map(|account| account.pubkey)
            .collect::<Vec<_>>();

        task_info_fetcher
            .get_base_accounts(&diffable_pubkeys, min_context_slot)
            .await
    }

    pub fn create_commit_finalize_task(
        commit_id: u64,
        allow_undelegation: bool,
        account: CommittedAccount,
        base_account: Option<Account>,
    ) -> CommitFinalizeTask {
        let base_account =
            if account.account.data.len() > COMMIT_STATE_SIZE_THRESHOLD {
                base_account
            } else {
                None
            };

        let delivery_details = if let Some(base_account) = base_account {
            CommitDelivery::DiffInArgs { base_account }
        } else {
            CommitDelivery::StateInArgs
        };

        CommitFinalizeTask {
            commit_id,
            allow_undelegation,
            committed_account: account,
            delivery: delivery_details,
        }
    }

    /// Returns [`BaseTaskImpl`]s for Commit stage
    pub async fn commit_tasks_with_mode<
        C: TaskInfoFetcher,
        P: IntentPersister,
    >(
        task_info_fetcher: &Arc<C>,
        intent_bundle: &ScheduledIntentBundle,
        persister: &Option<P>,
        mode: CommitExecutionMode,
    ) -> TaskBuilderResult<Vec<BaseTaskImpl>> {
        let mut tasks = Vec::new();
        tasks.extend(Self::create_action_tasks(
            intent_bundle.standalone_actions().as_slice(),
        ));

        let committed_accounts = intent_bundle.get_all_committed_accounts();

        // Get commit nonces and base accounts
        let min_context_slot = committed_accounts
            .iter()
            .map(|account| account.remote_slot)
            .max()
            .unwrap_or(0);
        let (commit_ids, base_accounts) = tokio::join!(
            Self::fetch_commit_nonces(
                task_info_fetcher,
                &committed_accounts,
                min_context_slot
            ),
            Self::fetch_diffable_accounts(
                task_info_fetcher,
                &committed_accounts,
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
        for (pubkey, commit_id) in &commit_ids {
            if let Err(err) =
                persister.set_commit_id(intent_bundle.id, pubkey, *commit_id)
            {
                error!(
                    intent_id = intent_bundle.id,
                    pubkey = %pubkey,
                    error = ?err,
                    "Failed to persist commit id"
                );
            }
        }

        fn create_commit_finalize_task(
            intent_id: u64,
            commit_ids: &HashMap<Pubkey, u64>,
            base_accounts: &mut HashMap<Pubkey, Account>,
            allow_undelegation: bool,
            account: &CommittedAccount,
        ) -> BaseTaskImpl {
            let commit_id =
                commit_ids.get(&account.pubkey).copied().unwrap_or_else(|| {
                    // This shall not ever happen since TaskInfoFetcher
                    // returns commit ids for all pubkeys or throws
                    // If it does occur, it will be patched and retried by IntentExecutor
                    error!(
                        intent_id,
                        pubkey = %account.pubkey,
                        "Commit id absent for pubkey"
                    );
                    0
                });
            let base_account = base_accounts.remove(&account.pubkey);

            TaskBuilderImpl::create_commit_finalize_task(
                commit_id,
                allow_undelegation,
                account.clone(),
                base_account,
            )
            .into()
        }

        fn create_commit_task(
            intent_id: u64,
            commit_ids: &HashMap<Pubkey, u64>,
            base_accounts: &mut HashMap<Pubkey, Account>,
            allow_undelegation: bool,
            account: &CommittedAccount,
        ) -> BaseTaskImpl {
            let commit_id =
                commit_ids.get(&account.pubkey).copied().unwrap_or_else(|| {
                    // This shall not ever happen since TaskInfoFetcher
                    // returns commit ids for all pubkeys or throws
                    // If it does occur, it will be patched and retried by IntentExecutor
                    error!(
                        intent_id,
                        pubkey = %account.pubkey,
                        "Commit id absent for pubkey"
                    );
                    0
                });
            let base_account = base_accounts.remove(&account.pubkey);

            TaskBuilderImpl::create_commit_task(
                commit_id,
                allow_undelegation,
                account.clone(),
                base_account,
            )
            .into()
        }

        #[allow(clippy::too_many_arguments)]
        fn extend_commit_tasks(
            tasks: &mut Vec<BaseTaskImpl>,
            commit: &CommitType,
            allow_undelegation: bool,
            intent_id: u64,
            commit_ids: &HashMap<Pubkey, u64>,
            base_accounts: &mut HashMap<Pubkey, Account>,
            use_commit_finalize: bool,
            include_base_actions: bool,
        ) {
            let committed_accounts = commit.get_committed_accounts();
            for account in committed_accounts {
                let task = if use_commit_finalize {
                    create_commit_finalize_task(
                        intent_id,
                        commit_ids,
                        base_accounts,
                        allow_undelegation,
                        account,
                    )
                } else {
                    create_commit_task(
                        intent_id,
                        commit_ids,
                        base_accounts,
                        allow_undelegation,
                        account,
                    )
                };
                tasks.push(task);
            }

            if include_base_actions {
                let CommitType::WithBaseActions { base_actions, .. } = commit
                else {
                    return;
                };
                tasks
                    .extend(TaskBuilderImpl::create_action_tasks(base_actions));
            }
        }

        let use_commit_finalize = mode == CommitExecutionMode::CommitFinalize;

        if let Some(ref value) = intent_bundle.intent_bundle.commit {
            extend_commit_tasks(
                &mut tasks,
                value,
                false,
                intent_bundle.id,
                &commit_ids,
                &mut base_accounts,
                use_commit_finalize,
                use_commit_finalize,
            );
        }

        if let Some(ref value) =
            intent_bundle.intent_bundle.commit_and_undelegate
        {
            extend_commit_tasks(
                &mut tasks,
                &value.commit_action,
                true,
                intent_bundle.id,
                &commit_ids,
                &mut base_accounts,
                use_commit_finalize,
                use_commit_finalize,
            );
        }

        if let Some(ref value) = intent_bundle.intent_bundle.commit_finalize {
            extend_commit_tasks(
                &mut tasks,
                value,
                false,
                intent_bundle.id,
                &commit_ids,
                &mut base_accounts,
                true,
                true,
            );
        }

        if let Some(ref value) =
            intent_bundle.intent_bundle.commit_finalize_and_undelegate
        {
            extend_commit_tasks(
                &mut tasks,
                &value.commit_action,
                true,
                intent_bundle.id,
                &commit_ids,
                &mut base_accounts,
                true,
                true,
            );
        }

        Ok(tasks)
    }

    /// Returns [`Task`]s for Finalize stage
    pub async fn finalize_tasks_with_mode<C: TaskInfoFetcher>(
        info_fetcher: &Arc<C>,
        intent_bundle: &ScheduledIntentBundle,
        mode: CommitExecutionMode,
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
        fn create_finalize_tasks(commit: &CommitType) -> Vec<BaseTaskImpl> {
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
                    tasks.extend(TaskBuilderImpl::create_action_tasks(
                        base_actions,
                    ));
                    tasks
                }
            }
        }

        async fn create_undelegate_tasks<C: TaskInfoFetcher>(
            commit_and_undelegate: &CommitAndUndelegate,
            info_fetcher: &Arc<C>,
        ) -> TaskBuilderResult<Vec<BaseTaskImpl>> {
            // Get rent reimbursments for undelegated accounts
            let accounts = commit_and_undelegate.get_committed_accounts();
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

            let mut tasks = accounts
                .iter()
                .zip(rent_reimbursements)
                .map(|(account, rent_reimbursement)| {
                    undelegate_task(account, &rent_reimbursement)
                })
                .collect::<Vec<_>>();

            if let UndelegateType::WithBaseActions(actions) =
                &commit_and_undelegate.undelegate_action
            {
                tasks.extend(TaskBuilderImpl::create_action_tasks(actions));
            }
            Ok(tasks)
        }

        let mut tasks = Vec::new();
        let mut futures = Vec::with_capacity(2);

        if mode == CommitExecutionMode::SeparateCommitAndFinalize {
            if let Some(ref value) = intent_bundle.intent_bundle.commit {
                tasks.extend(create_finalize_tasks(value));
            }
        }

        if let Some(ref value) =
            intent_bundle.intent_bundle.commit_and_undelegate
        {
            if mode == CommitExecutionMode::SeparateCommitAndFinalize {
                tasks.extend(create_finalize_tasks(&value.commit_action));
            }
            futures.push(create_undelegate_tasks(value, info_fetcher));
        }

        if let Some(ref value) =
            intent_bundle.intent_bundle.commit_finalize_and_undelegate
        {
            futures.push(create_undelegate_tasks(value, info_fetcher));
        }

        tasks.extend(try_join_all(futures).await?.into_iter().flatten());

        Ok(tasks)
    }
}

#[async_trait]
impl TasksBuilder for TaskBuilderImpl {
    async fn commit_tasks<C: TaskInfoFetcher, P: IntentPersister>(
        task_info_fetcher: &Arc<C>,
        intent_bundle: &ScheduledIntentBundle,
        persister: &Option<P>,
    ) -> TaskBuilderResult<Vec<BaseTaskImpl>> {
        Self::commit_tasks_with_mode(
            task_info_fetcher,
            intent_bundle,
            persister,
            DEFAULT_COMMIT_EXECUTION_MODE,
        )
        .await
    }

    async fn finalize_tasks<C: TaskInfoFetcher>(
        info_fetcher: &Arc<C>,
        intent_bundle: &ScheduledIntentBundle,
    ) -> TaskBuilderResult<Vec<BaseTaskImpl>> {
        Self::finalize_tasks_with_mode(
            info_fetcher,
            intent_bundle,
            DEFAULT_COMMIT_EXECUTION_MODE,
        )
        .await
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

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, sync::Arc};

    use solana_account::Account;
    use solana_pubkey::Pubkey;

    use super::*;
    use crate::{
        intent_execution_manager::intent_scheduler::create_test_intent,
        intent_executor::task_info_fetcher::{
            TaskInfoFetcher, TaskInfoFetcherResult,
        },
        persist::IntentPersisterImpl,
    };

    struct MockInfoFetcher;

    #[async_trait::async_trait]
    impl TaskInfoFetcher for MockInfoFetcher {
        async fn fetch_next_commit_nonces(
            &self,
            pubkeys: &[Pubkey],
            _: u64,
        ) -> TaskInfoFetcherResult<HashMap<Pubkey, u64>> {
            Ok(pubkeys
                .iter()
                .enumerate()
                .map(|(index, pubkey)| (*pubkey, index as u64))
                .collect())
        }

        async fn fetch_current_commit_nonces(
            &self,
            pubkeys: &[Pubkey],
            _: u64,
        ) -> TaskInfoFetcherResult<HashMap<Pubkey, u64>> {
            Ok(pubkeys.iter().map(|pubkey| (*pubkey, 0)).collect())
        }

        async fn fetch_rent_reimbursements(
            &self,
            pubkeys: &[Pubkey],
            _: u64,
        ) -> TaskInfoFetcherResult<Vec<Pubkey>> {
            Ok(pubkeys.iter().map(|_| Pubkey::new_unique()).collect())
        }

        async fn get_base_accounts(
            &self,
            _: &[Pubkey],
            _: u64,
        ) -> TaskInfoFetcherResult<HashMap<Pubkey, Account>> {
            Ok(Default::default())
        }
    }

    #[tokio::test]
    async fn commit_intent_uses_commit_finalize_without_finalize_task() {
        let pubkey = Pubkey::new_unique();
        let intent = create_test_intent(1, &[pubkey], false);
        let info_fetcher = Arc::new(MockInfoFetcher);

        let commit_tasks = TaskBuilderImpl::commit_tasks(
            &info_fetcher,
            &intent,
            &None::<IntentPersisterImpl>,
        )
        .await
        .unwrap();
        let finalize_tasks =
            TaskBuilderImpl::finalize_tasks(&info_fetcher, &intent)
                .await
                .unwrap();

        assert!(matches!(
            commit_tasks.as_slice(),
            [BaseTaskImpl::CommitFinalize(task)]
                if task.committed_account.pubkey == pubkey
                    && !task.allow_undelegation
        ));
        assert!(finalize_tasks.is_empty());
    }

    #[tokio::test]
    async fn commit_and_undelegate_uses_commit_finalize_then_undelegate() {
        let pubkey = Pubkey::new_unique();
        let intent = create_test_intent(1, &[pubkey], true);
        let info_fetcher = Arc::new(MockInfoFetcher);

        let commit_tasks = TaskBuilderImpl::commit_tasks(
            &info_fetcher,
            &intent,
            &None::<IntentPersisterImpl>,
        )
        .await
        .unwrap();
        let finalize_tasks =
            TaskBuilderImpl::finalize_tasks(&info_fetcher, &intent)
                .await
                .unwrap();

        assert!(matches!(
            commit_tasks.as_slice(),
            [BaseTaskImpl::CommitFinalize(task)]
                if task.committed_account.pubkey == pubkey
                    && task.allow_undelegation
        ));
        assert!(matches!(
            finalize_tasks.as_slice(),
            [BaseTaskImpl::Undelegate(task)]
                if task.delegated_account == pubkey
        ));
    }

    #[tokio::test]
    async fn separate_commit_mode_uses_commit_then_finalize_tasks() {
        let pubkey = Pubkey::new_unique();
        let intent = create_test_intent(1, &[pubkey], false);
        let info_fetcher = Arc::new(MockInfoFetcher);

        let commit_tasks = TaskBuilderImpl::commit_tasks_with_mode(
            &info_fetcher,
            &intent,
            &None::<IntentPersisterImpl>,
            CommitExecutionMode::SeparateCommitAndFinalize,
        )
        .await
        .unwrap();
        let finalize_tasks = TaskBuilderImpl::finalize_tasks_with_mode(
            &info_fetcher,
            &intent,
            CommitExecutionMode::SeparateCommitAndFinalize,
        )
        .await
        .unwrap();

        assert!(matches!(
            commit_tasks.as_slice(),
            [BaseTaskImpl::Commit(task)]
                if task.committed_account.pubkey == pubkey
                    && !task.allow_undelegation
        ));
        assert!(matches!(
            finalize_tasks.as_slice(),
            [BaseTaskImpl::Finalize(task)]
                if task.delegated_account == pubkey
        ));
    }

    #[tokio::test]
    async fn separate_commit_mode_uses_finalize_then_undelegate() {
        let pubkey = Pubkey::new_unique();
        let intent = create_test_intent(1, &[pubkey], true);
        let info_fetcher = Arc::new(MockInfoFetcher);

        let commit_tasks = TaskBuilderImpl::commit_tasks_with_mode(
            &info_fetcher,
            &intent,
            &None::<IntentPersisterImpl>,
            CommitExecutionMode::SeparateCommitAndFinalize,
        )
        .await
        .unwrap();
        let finalize_tasks = TaskBuilderImpl::finalize_tasks_with_mode(
            &info_fetcher,
            &intent,
            CommitExecutionMode::SeparateCommitAndFinalize,
        )
        .await
        .unwrap();

        assert!(matches!(
            commit_tasks.as_slice(),
            [BaseTaskImpl::Commit(task)]
                if task.committed_account.pubkey == pubkey
                    && task.allow_undelegation
        ));
        assert!(matches!(
            finalize_tasks.as_slice(),
            [BaseTaskImpl::Finalize(finalize), BaseTaskImpl::Undelegate(undelegate)]
                if finalize.delegated_account == pubkey
                    && undelegate.delegated_account == pubkey
        ));
    }
}
