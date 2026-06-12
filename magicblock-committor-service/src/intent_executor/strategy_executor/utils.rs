use std::{ops::ControlFlow, time::Duration};

use async_trait::async_trait;
use magicblock_core::traits::{
    ActionError, ActionResult, ActionsCallbackScheduler,
};
use magicblock_program::outbox::ExecutionStage;
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_rpc_client::rpc_client::SerializableTransaction;
use solana_signature::Signature;
use solana_transaction::versioned::VersionedTransaction;
use tokio::time::timeout;
use tracing::{error, info, warn};

use crate::{
    intent_executor::{
        error::{
            IntentExecutorError, IntentExecutorResult,
            TransactionStrategyExecutionError,
        },
        intent_execution_client::IntentExecutionClient,
        strategy_executor::{
            patcher::Patcher,
            single_stage::SingleStageStrategyExecutor,
            two_stage::{Committed, Initialized, TwoStageStrategyExecutor},
        },
        task_info_fetcher::{CacheTaskInfoFetcher, ResetType, TaskInfoFetcher},
        IntentExecutionReport,
    },
    outbox_client::OutboxClient,
    tasks::{
        task_builder::TaskBuilderError,
        task_strategist::{TaskStrategist, TransactionStrategy},
        BaseTaskImpl,
    },
    transaction_preparator::{
        error::TransactionPreparatorError, TransactionPreparator,
    },
};

const STAGE_LOOP_CEILING: u8 = 10;

pub(in crate::intent_executor) struct ExecutionState<'a> {
    pub current_attempt: &'a mut u8,
    pub transaction_strategy: &'a mut TransactionStrategy,
    pub pending_signature: &'a mut Option<Signature>,
    pub execution_report: &'a mut IntentExecutionReport,
}

pub(in crate::intent_executor) async fn stage_execution_loop<'a, T, O, P>(
    authority: &Keypair,
    intent_client: &IntentExecutionClient,
    outbox_client: &O,
    transaction_preparator: &T,
    mut patcher: P,
    intent_id: u64,
    make_outbox_stage: impl Fn(Signature) -> ExecutionStage,
    map_preparation_err: fn(TransactionPreparatorError) -> IntentExecutorError,
    state: ExecutionState<'a>,
) -> IntentExecutorResult<Result<Signature, TransactionStrategyExecutionError>>
where
    T: TransactionPreparator,
    O: OutboxClient,
    O::Error: Into<IntentExecutorError>,
    P: Patcher,
{
    loop {
        if let Some(ref sig) = state.pending_signature {
            let flow = check_pending_signature(intent_client, sig).await?;
            // Pending signature corresponds to succeeded transaction - break
            if let ControlFlow::Break(()) = flow {
                break Ok(Ok(*sig));
            }

            *state.pending_signature = None;
        }

        *state.current_attempt += 1;

        // Prepare message
        let prepared_transaction = prepare_transaction(
            intent_client,
            authority,
            transaction_preparator,
            state.transaction_strategy,
        )
        .await
        .map_err(map_preparation_err)?;

        // Record in outbox before sending
        let signature = prepared_transaction.get_signature();
        outbox_client
            .set_intent_execution_stage(
                intent_id,
                make_outbox_stage(*signature),
            )
            .await
            .map_err(Into::into)?;

        // Now record locally signature of tx we about to send
        // Precaution for timeout in between
        *state.pending_signature = Some(*signature);

        // Send signed tx
        let execution_result = intent_client
            .send_signed_tx_with_retries(
                &prepared_transaction,
                &state.transaction_strategy.optimized_tasks,
            )
            .await;

        // Result returned, cleanup pending signature
        *state.pending_signature = None;

        let execution_err = match execution_result {
            Ok(()) => return Ok(Ok(*signature)),
            Err(err) => err,
        };
        let flow = patcher
            .patch(
                &execution_err,
                state.transaction_strategy,
                state.execution_report,
            )
            .await?;
        let cleanup = match flow {
            ControlFlow::Continue(value) => value,
            ControlFlow::Break(()) => return Ok(Err(execution_err)),
        };
        intent_client.invalidate_cached_blockhash().await;
        state.execution_report.dispose(cleanup);

        if *state.current_attempt >= STAGE_LOOP_CEILING {
            error!("CRITICAL! Recursion ceiling reached");
            return Ok(Err(execution_err));
        } else {
            state.execution_report.add_patched_error(execution_err);
        }
    }
}

pub async fn prepare_transaction<T: TransactionPreparator>(
    client: &IntentExecutionClient,
    authority: &Keypair,
    transaction_preparator: &T,
    transaction_strategy: &mut TransactionStrategy,
) -> Result<VersionedTransaction, TransactionPreparatorError> {
    let mut prepared_message = transaction_preparator
        .prepare_for_strategy(&authority, transaction_strategy)
        .await?;

    // Get latest blockhash(Part of preparation I guess)
    let blockhash = client
        .get_latest_blockhash()
        .await
        .map_err(TransactionPreparatorError::GetLatestBlockhashError)?;
    prepared_message.set_recent_blockhash(blockhash);

    // Create and sign transaction(Part of transaction preparation i guess, hence the errors)
    let transaction =
        VersionedTransaction::try_new(prepared_message, &[&authority])
            .map_err(TransactionPreparatorError::SignerError)?;
    Ok(transaction)
}

/// Checks a `pending_signature` loaded from the outbox against the full
/// transaction history. Returns `Break(sig)` if the tx succeeded — the caller
/// can short-circuit and skip re-execution. Returns `Continue` for every other
/// outcome (tx failed, never sent, or RPC returned no entry), letting the
/// normal execution loop proceed.
pub(in crate::intent_executor) async fn check_pending_signature(
    client: &IntentExecutionClient,
    sig: &Signature,
) -> IntentExecutorResult<ControlFlow<()>> {
    let statuses = client
        .get_signature_statuses_with_history(std::slice::from_ref(sig))
        .await
        .map_err(IntentExecutorError::GetPendingSignatureStatusError)?;

    match statuses.get(0) {
        Some(Some(Ok(()))) => Ok(ControlFlow::Break(())),
        // TODO(edwin): well, that is bizarre one
        None => {
            warn!(pending_signature = ?sig, "RPC did not return status for signature");
            Ok(ControlFlow::Continue(()))
        }
        // Any case below means tx failed in execution we continue attempts
        Some(None) => {
            info!(pending_signature = ?sig, "Transaction corresponding to pending signature was never sent");
            Ok(ControlFlow::Continue(()))
        }
        Some(Some(Err(err))) => {
            info!(pending_signature = ?sig, error = ?err, "Transaction corresponding to pending signature failed");
            Ok(ControlFlow::Continue(()))
        }
    }
}

pub async fn prepare_and_execute_strategy<T>(
    client: &IntentExecutionClient,
    authority: &Keypair,
    transaction_preparator: &T,
    transaction_strategy: &mut TransactionStrategy,
) -> Result<
    Result<Signature, TransactionStrategyExecutionError>,
    TransactionPreparatorError,
>
where
    T: TransactionPreparator,
{
    let prepared_message = transaction_preparator
        .prepare_for_strategy(authority, transaction_strategy)
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
    let min_context_slot = strategy
        .optimized_tasks
        .iter()
        .filter_map(|task| match task {
            BaseTaskImpl::Commit(task) => {
                Some(task.committed_account.remote_slot)
            }
            BaseTaskImpl::CommitFinalize(task) => {
                Some(task.committed_account.remote_slot)
            }
            _ => None,
        })
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
    for task in &mut strategy.optimized_tasks {
        match task {
            BaseTaskImpl::Commit(task) => {
                let Some(commit_id) =
                    commit_ids.get(&task.committed_account.pubkey)
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
            BaseTaskImpl::CommitFinalize(task) => {
                let Some(commit_id) =
                    commit_ids.get(&task.committed_account.pubkey)
                else {
                    continue;
                };
                if commit_id == &task.commit_id {
                    continue;
                }

                // Handle invalid tasks
                to_cleanup.push(BaseTaskImpl::CommitFinalize(task.clone()));
                task.reset_commit_id(*commit_id);
            }
            _ => {}
        }
    }

    let old_alts = strategy.dummy_revaluate_alts(authority);
    Ok(TransactionStrategy {
        optimized_tasks: to_cleanup,
        lookup_tables_keys: old_alts,
        standalone_action_nonce: None,
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
        standalone_action_nonce: None,
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
        standalone_action_nonce: None,
    };

    // We clean up only ALTs
    let to_cleanup = TransactionStrategy {
        optimized_tasks: vec![],
        lookup_tables_keys: strategy.lookup_tables_keys,
        standalone_action_nonce: None,
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
            standalone_action_nonce: None,
        }
    } else {
        TransactionStrategy {
            optimized_tasks: vec![],
            lookup_tables_keys: vec![],
            standalone_action_nonce: None,
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

// TODO(edwin): docs
pub(in crate::intent_executor) async fn execute_with_timeout(
    time_left: Option<Duration>,
    mut executor: impl StageExecutor,
) -> IntentExecutorResult<Signature> {
    if executor.has_callbacks() {
        if let Some(time_left) = time_left {
            match timeout(time_left, executor.execute()).await {
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

    // Timeout concerns only callbacks
    // We still need to drive intent to completion
    executor.execute().await
}

#[async_trait]
pub(in crate::intent_executor) trait StageExecutor {
    fn has_callbacks(&self) -> bool;
    async fn execute(&mut self) -> IntentExecutorResult<Signature>;
    fn execute_callbacks(
        &mut self,
        signature: Option<Signature>,
        result: ActionResult,
    );
}

pub(in crate::intent_executor) struct SingleStage<'a, 'e, A, T, F, O> {
    pub(in crate::intent_executor) inner:
        &'a mut SingleStageStrategyExecutor<'e, F, A, O>,
    pub(in crate::intent_executor) transaction_preparator: &'a T,
    pub(in crate::intent_executor) committed_pubkeys: &'a [Pubkey],
}

#[async_trait]
impl<'a, 'e, A, T, F, O> StageExecutor for SingleStage<'a, 'e, A, T, F, O>
where
    A: ActionsCallbackScheduler,
    T: TransactionPreparator,
    F: TaskInfoFetcher,
    O: OutboxClient,
    O::Error: Into<IntentExecutorError>,
{
    fn has_callbacks(&self) -> bool {
        self.inner.has_callbacks()
    }

    async fn execute(&mut self) -> IntentExecutorResult<Signature> {
        self.inner
            .execute(self.committed_pubkeys, self.transaction_preparator)
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

pub(in crate::intent_executor) struct CommitStage<'a, 'e, A, T, F, O> {
    pub(in crate::intent_executor) inner:
        &'a mut TwoStageStrategyExecutor<'e, A, O, Initialized>,
    pub(in crate::intent_executor) transaction_preparator: &'a T,
    pub(in crate::intent_executor) task_info_fetcher:
        &'a CacheTaskInfoFetcher<F>,
    pub(in crate::intent_executor) committed_pubkeys: &'a [Pubkey],
}

#[async_trait]
impl<'a, 'e, A, T, F, O> StageExecutor for CommitStage<'a, 'e, A, T, F, O>
where
    A: ActionsCallbackScheduler,
    T: TransactionPreparator,
    F: TaskInfoFetcher,
    O: OutboxClient,
    O::Error: Into<IntentExecutorError>,
{
    fn has_callbacks(&self) -> bool {
        self.inner.has_callbacks()
    }

    async fn execute(&mut self) -> IntentExecutorResult<Signature> {
        self.inner
            .commit(
                self.committed_pubkeys,
                self.transaction_preparator,
                self.task_info_fetcher,
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

pub(in crate::intent_executor) struct FinalizeStage<'a, 'e, A, T, O> {
    pub(in crate::intent_executor) inner:
        &'a mut TwoStageStrategyExecutor<'e, A, O, Committed>,
    pub(in crate::intent_executor) transaction_preparator: &'a T,
}

#[async_trait]
impl<'a, 'e, A, T, O> StageExecutor for FinalizeStage<'a, 'e, A, T, O>
where
    A: ActionsCallbackScheduler,
    T: TransactionPreparator,
    O: OutboxClient,
    O::Error: Into<IntentExecutorError>,
{
    fn has_callbacks(&self) -> bool {
        self.inner.has_callbacks()
    }

    async fn execute(&mut self) -> IntentExecutorResult<Signature> {
        self.inner.finalize(self.transaction_preparator).await
    }

    fn execute_callbacks(
        &mut self,
        signature: Option<Signature>,
        result: ActionResult,
    ) {
        self.inner.execute_callbacks(signature, result)
    }
}
