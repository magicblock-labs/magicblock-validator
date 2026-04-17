use std::time::Duration;

use async_trait::async_trait;
use magicblock_core::traits::{
    ActionError, ActionResult, ActionsCallbackScheduler,
};
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_signature::Signature;
use tokio::time::timeout;
use tracing::info;

use crate::{
    intent_executor::{
        error::{IntentExecutorResult, TransactionStrategyExecutionError},
        intent_execution_client::IntentExecutionClient,
        single_stage_executor::SingleStageExecutor,
        task_info_fetcher::{CacheTaskInfoFetcher, ResetType, TaskInfoFetcher},
        two_stage_executor::{Committed, Initialized, TwoStageExecutor},
        IntentExecutionReport,
    },
    persist::IntentPersister,
    tasks::{
        task_builder::TaskBuilderError,
        task_strategist::{TaskStrategist, TransactionStrategy},
        BaseTaskImpl,
    },
    transaction_preparator::{
        error::TransactionPreparatorError, TransactionPreparator,
    },
};

pub async fn prepare_and_execute_strategy<P, T>(
    client: &IntentExecutionClient,
    authority: &Keypair,
    transaction_preparator: &T,
    transaction_strategy: &mut TransactionStrategy,
    persister: &Option<P>,
) -> IntentExecutorResult<
    IntentExecutorResult<Signature, TransactionStrategyExecutionError>,
    TransactionPreparatorError,
>
where
    T: TransactionPreparator,
    P: IntentPersister,
{
    let prepared_message = transaction_preparator
        .prepare_for_strategy(authority, transaction_strategy, persister)
        .await?;

    let execution_result = client
        .execute_message_with_retries(
            authority,
            prepared_message,
            &transaction_strategy.optimized_tasks,
        )
        .await;

    Ok(execution_result)
}

/// Handles out of sync commit id error, fixes current strategy
/// Returns strategy to be cleaned up
/// TODO(edwin): TransactionStrategy -> CleanupStrategy or something, naming is confusing for something that is cleaned up
pub(in crate::intent_executor) async fn handle_commit_id_error<
    T: TaskInfoFetcher,
>(
    authority: &Pubkey,
    task_info_fetcher: &CacheTaskInfoFetcher<T>,
    committed_pubkeys: &[Pubkey],
    strategy: &mut TransactionStrategy,
) -> Result<TransactionStrategy, TaskBuilderError> {
    let commit_tasks: Vec<_> = strategy
        .optimized_tasks
        .iter_mut()
        .filter_map(|task| {
            if let BaseTaskImpl::Commit(commit_task) = task {
                Some(commit_task)
            } else {
                None
            }
        })
        .collect();
    let min_context_slot = commit_tasks
        .iter()
        .map(|task| task.committed_account.remote_slot)
        .max()
        .unwrap_or_default();

    // We reset TaskInfoFetcher for all committed accounts
    // We re-fetch them to fix out of sync tasks
    task_info_fetcher.reset(ResetType::Specific(committed_pubkeys));
    let commit_ids = task_info_fetcher
        .fetch_next_commit_nonces(committed_pubkeys, min_context_slot)
        .await
        .map_err(TaskBuilderError::CommitTasksBuildError)?;

    // Here we find the broken tasks and reset them
    // Broken tasks are prepared incorrectly so they have to be cleaned up
    let mut to_cleanup = Vec::new();
    for task in commit_tasks {
        let Some(commit_id) = commit_ids.get(&task.committed_account.pubkey)
        else {
            continue;
        };
        if commit_id == &task.commit_id {
            continue;
        }

        // Handle invalid tasks
        to_cleanup.push(BaseTaskImpl::Commit(task.clone()));
        task.reset_commit_id(*commit_id);
    }

    let old_alts = strategy.dummy_revaluate_alts(authority);
    Ok(TransactionStrategy {
        optimized_tasks: to_cleanup,
        lookup_tables_keys: old_alts,
    })
}

/// Handle CPI limit error, splits single strategy flow into 2
/// Returns Commit stage strategy, Finalize stage strategy and strategy to clean up
pub(in crate::intent_executor) fn handle_cpi_limit_error(
    authority: &Pubkey,
    strategy: TransactionStrategy,
) -> (
    TransactionStrategy,
    TransactionStrategy,
    TransactionStrategy,
) {
    // We encountered error "Max instruction trace length exceeded"
    // All the tasks a prepared to be executed at this point
    // We attempt Two stages commit flow, need to split tasks up
    let last_commit_ind = strategy.optimized_tasks.iter().rposition(|el| {
        matches!(
            el,
            BaseTaskImpl::Commit(_) | BaseTaskImpl::CommitFinalize(_)
        )
    });
    let (mut commit_stage_tasks, mut finalize_stage_tasks) = (vec![], vec![]);
    for (i, el) in strategy.optimized_tasks.into_iter().enumerate() {
        if Some(i) <= last_commit_ind {
            commit_stage_tasks.push(el);
        } else {
            finalize_stage_tasks.push(el);
        }
    }

    let commit_alt_pubkeys = if strategy.lookup_tables_keys.is_empty() {
        vec![]
    } else {
        TaskStrategist::collect_lookup_table_keys(
            authority,
            &commit_stage_tasks,
        )
    };
    let commit_strategy = TransactionStrategy {
        optimized_tasks: commit_stage_tasks,
        lookup_tables_keys: commit_alt_pubkeys,
    };

    let finalize_alt_pubkeys = if strategy.lookup_tables_keys.is_empty() {
        vec![]
    } else {
        TaskStrategist::collect_lookup_table_keys(
            authority,
            &finalize_stage_tasks,
        )
    };
    let finalize_strategy = TransactionStrategy {
        optimized_tasks: finalize_stage_tasks,
        lookup_tables_keys: finalize_alt_pubkeys,
    };

    // We clean up only ALTs
    let to_cleanup = TransactionStrategy {
        optimized_tasks: vec![],
        lookup_tables_keys: strategy.lookup_tables_keys,
    };

    (commit_strategy, finalize_strategy, to_cleanup)
}

/// Handles undelegation error, stripping away actions
/// Returns [`TransactionStrategy`] to be cleaned up
pub(in crate::intent_executor) fn handle_undelegation_error(
    authority: &Pubkey,
    strategy: &mut TransactionStrategy,
) -> TransactionStrategy {
    let position = strategy
        .optimized_tasks
        .iter()
        .position(|el| matches!(el, BaseTaskImpl::Undelegate(_)));

    if let Some(position) = position {
        // Remove everything after undelegation including post undelegation actions
        let removed_task = strategy.optimized_tasks.drain(position..).collect();
        let old_alts = strategy.dummy_revaluate_alts(authority);
        TransactionStrategy {
            optimized_tasks: removed_task,
            lookup_tables_keys: old_alts,
        }
    } else {
        TransactionStrategy {
            optimized_tasks: vec![],
            lookup_tables_keys: vec![],
        }
    }
}

pub(in crate::intent_executor) fn handle_actions_result<A>(
    authority: &Pubkey,
    callback_scheduler: &A,
    execution_report: &mut IntentExecutionReport,
    transaction_strategy: &mut TransactionStrategy,
    signature: Option<Signature>,
    result: ActionResult,
) -> TransactionStrategy
where
    A: ActionsCallbackScheduler,
{
    let (callbacks, junk) = if result.is_ok() {
        let callbacks = transaction_strategy.extract_action_callbacks();
        (callbacks, TransactionStrategy::default())
    } else {
        let mut removed_actions =
            transaction_strategy.remove_actions(authority);
        let callbacks = removed_actions.extract_action_callbacks();
        (callbacks, removed_actions)
    };
    if !callbacks.is_empty() {
        let result = callback_scheduler.schedule(callbacks, signature, result);
        execution_report.add_callback_report(result);
    }

    junk
}

pub(in crate::intent_executor) async fn execute_with_timeout<
    P: IntentPersister,
>(
    time_left: Option<Duration>,
    mut executor: impl StageExecutor,
    persister: &Option<P>,
) -> IntentExecutorResult<Signature> {
    if executor.has_callbacks() {
        if let Some(time_left) = time_left {
            match timeout(time_left, executor.execute(persister)).await {
                Ok(res) => return res,
                Err(_) => {
                    // The race between callback and intent txn is handled
                    // on the user smart contract side via TimeoutError.
                    // We must respect the timeout contract.
                    info!("Intent execution timed out, cleaning up actions");
                    executor.execute_callbacks(
                        None,
                        Err(ActionError::TimeoutError),
                    );
                }
            }
        } else {
            // Already timed out; see comment above.
            executor.execute_callbacks(None, Err(ActionError::TimeoutError));
        }
    }

    executor.execute(persister).await
}

#[async_trait]
pub(in crate::intent_executor) trait StageExecutor {
    fn has_callbacks(&self) -> bool;
    async fn execute<P: IntentPersister>(
        &mut self,
        persister: &Option<P>,
    ) -> IntentExecutorResult<Signature>;
    fn execute_callbacks(
        &mut self,
        signature: Option<Signature>,
        result: ActionResult,
    );
}

pub(in crate::intent_executor) struct SingleStage<'a, 'e, A, T, F> {
    pub(in crate::intent_executor) inner: &'a mut SingleStageExecutor<'e, F, A>,
    pub(in crate::intent_executor) transaction_preparator: &'a T,
    pub(in crate::intent_executor) committed_pubkeys: &'a [Pubkey],
}

#[async_trait]
impl<'a, 'e, A, T, F> StageExecutor for SingleStage<'a, 'e, A, T, F>
where
    A: ActionsCallbackScheduler,
    T: TransactionPreparator,
    F: TaskInfoFetcher,
{
    fn has_callbacks(&self) -> bool {
        self.inner.has_callbacks()
    }

    async fn execute<P: IntentPersister>(
        &mut self,
        persister: &Option<P>,
    ) -> IntentExecutorResult<Signature> {
        self.inner
            .execute(
                self.committed_pubkeys,
                self.transaction_preparator,
                persister,
            )
            .await
    }

    fn execute_callbacks(
        &mut self,
        signature: Option<Signature>,
        result: ActionResult,
    ) {
        self.inner.execute_callbacks(signature, result)
    }
}

pub(in crate::intent_executor) struct CommitStage<'a, 'e, A, T, F> {
    pub(in crate::intent_executor) inner:
        &'a mut TwoStageExecutor<'e, A, Initialized>,
    pub(in crate::intent_executor) transaction_preparator: &'a T,
    pub(in crate::intent_executor) task_info_fetcher:
        &'a CacheTaskInfoFetcher<F>,
    pub(in crate::intent_executor) committed_pubkeys: &'a [Pubkey],
}

#[async_trait]
impl<'a, 'e, A, T, F> StageExecutor for CommitStage<'a, 'e, A, T, F>
where
    A: ActionsCallbackScheduler,
    T: TransactionPreparator,
    F: TaskInfoFetcher,
{
    fn has_callbacks(&self) -> bool {
        self.inner.has_callbacks()
    }

    async fn execute<P: IntentPersister>(
        &mut self,
        persister: &Option<P>,
    ) -> IntentExecutorResult<Signature> {
        self.inner
            .commit(
                self.committed_pubkeys,
                self.transaction_preparator,
                self.task_info_fetcher,
                persister,
            )
            .await
    }

    fn execute_callbacks(
        &mut self,
        signature: Option<Signature>,
        result: ActionResult,
    ) {
        self.inner.execute_callbacks(signature, result)
    }
}

pub(in crate::intent_executor) struct FinalizeStage<'a, 'e, A, T> {
    pub(in crate::intent_executor) inner:
        &'a mut TwoStageExecutor<'e, A, Committed>,
    pub(in crate::intent_executor) transaction_preparator: &'a T,
}

#[async_trait]
impl<'a, 'e, A, T> StageExecutor for FinalizeStage<'a, 'e, A, T>
where
    A: ActionsCallbackScheduler,
    T: TransactionPreparator,
{
    fn has_callbacks(&self) -> bool {
        self.inner.has_callbacks()
    }

    async fn execute<P: IntentPersister>(
        &mut self,
        persister: &Option<P>,
    ) -> IntentExecutorResult<Signature> {
        self.inner
            .finalize(self.transaction_preparator, persister)
            .await
    }

    fn execute_callbacks(
        &mut self,
        signature: Option<Signature>,
        result: ActionResult,
    ) {
        self.inner.execute_callbacks(signature, result)
    }
}
