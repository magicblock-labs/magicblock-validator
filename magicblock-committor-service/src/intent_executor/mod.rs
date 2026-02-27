pub mod error;
pub(crate) mod intent_executor_factory;
pub mod single_stage_executor;
pub mod task_info_fetcher;
pub mod two_stage_executor;

use std::{
    mem,
    ops::ControlFlow,
    sync::Arc,
    time::{Duration, Instant},
};

use async_trait::async_trait;
use futures_util::future::{join, try_join_all};
use magicblock_metrics::metrics;
use magicblock_program::{
    magic_scheduled_base_intent::{BaseActionCallback, ScheduledIntentBundle},
    validator::validator_authority,
};
use magicblock_rpc_client::{
    utils::{
        decide_rpc_error_flow, map_magicblock_client_error,
        send_transaction_with_retries, SendErrorMapper, TransactionErrorMapper,
    },
    MagicBlockRpcClientError, MagicBlockSendTransactionConfig,
    MagicBlockSendTransactionOutcome, MagicblockRpcClient,
};
use solana_keypair::Keypair;
use solana_message::VersionedMessage;
use solana_pubkey::Pubkey;
use solana_rpc_client_api::config::RpcTransactionConfig;
use solana_signature::Signature;
use solana_signer::Signer;
use solana_transaction::versioned::VersionedTransaction;
use tokio::time::timeout;
use tracing::{info, trace, warn};

use crate::{
    intent_executor::{
        error::{
            IntentExecutorError, IntentExecutorResult,
            IntentTransactionErrorMapper, InternalError,
            TransactionStrategyExecutionError,
        },
        single_stage_executor::SingleStageExecutor,
        task_info_fetcher::{ResetType, TaskInfoFetcher},
        two_stage_executor::TwoStageExecutor,
    },
    persist::{CommitStatus, CommitStatusSignatures, IntentPersister},
    tasks::{
        task_builder::{TaskBuilderError, TaskBuilderImpl, TasksBuilder},
        task_strategist::{
            StrategyExecutionMode, TaskStrategist, TransactionStrategy,
        },
        BaseTaskImpl,
    },
    transaction_preparator::{
        delivery_preparator::BufferExecutionError,
        error::TransactionPreparatorError, TransactionPreparator,
    },
    utils::persist_status_update_by_message_set,
};

#[derive(Clone, Copy, Debug)]
pub enum ExecutionOutput {
    // TODO: with arrival of challenge window remove SingleStage
    // Protocol requires 2 stage: Commit, Finalize
    // SingleStage - optimization for timebeing
    SingleStage(Signature),
    TwoStage {
        /// Commit stage signature
        commit_signature: Signature,
        /// Finalize stage signature
        finalize_signature: Signature,
    },
}

impl metrics::LabelValue for ExecutionOutput {
    fn value(&self) -> &str {
        match self {
            Self::SingleStage(_) => "single_stage_succeeded",
            Self::TwoStage {
                commit_signature: _,
                finalize_signature: _,
            } => "two_stage_succeeded",
        }
    }
}

pub struct IntentExecutionResult {
    /// Final result of Intent Execution
    pub inner: IntentExecutorResult<ExecutionOutput>,
    /// Errors patched along the way
    pub patched_errors: Vec<TransactionStrategyExecutionError>,
}

#[async_trait]
pub trait IntentExecutor: Send + Sync + 'static {
    /// Executes Message on Base layer
    /// Returns `ExecutionOutput` or an `Error`
    async fn execute<P: IntentPersister>(
        &mut self,
        base_intent: ScheduledIntentBundle,
        persister: Option<P>,
    ) -> IntentExecutionResult;

    /// Cleans up after intent
    async fn cleanup(self) -> Result<(), BufferExecutionError>;
}

pub trait ActionsCallbackExecutor: Send + Sync + Clone + 'static {
    /// Executes actions callbacks
    fn execute(&self, callbacks: Vec<BaseActionCallback>, succeeded: bool);
}

pub struct IntentExecutorImpl<T, F, A> {
    authority: Keypair,
    rpc_client: MagicblockRpcClient,
    transaction_preparator: T,
    task_info_fetcher: Arc<F>,
    actions_callback_executor: A,
    /// Timeout for Intent's actions
    actions_timeout: Duration,

    /// Intent execution started at
    pub started_at: Instant,
    /// Junk that needs to be cleaned up
    pub junk: Vec<TransactionStrategy>,
    /// Errors we patched trying to recover intent
    pub patched_errors: Vec<TransactionStrategyExecutionError>,
}

impl<T, F, A> IntentExecutorImpl<T, F, A>
where
    T: TransactionPreparator,
    F: TaskInfoFetcher,
    A: ActionsCallbackExecutor,
{
    pub fn new(
        rpc_client: MagicblockRpcClient,
        transaction_preparator: T,
        task_info_fetcher: Arc<F>,
        actions_callback_executor: A,
        actions_timeout: Duration,
    ) -> Self {
        let authority = validator_authority();
        Self {
            authority,
            rpc_client,
            transaction_preparator,
            task_info_fetcher,
            actions_callback_executor,
            actions_timeout,

            started_at: Instant::now(),
            junk: vec![],
            patched_errors: vec![],
        }
    }

    async fn execute_inner<P: IntentPersister>(
        &mut self,
        intent_bundle: ScheduledIntentBundle,
        persister: &Option<P>,
    ) -> IntentExecutorResult<ExecutionOutput> {
        if intent_bundle.is_empty() {
            return Err(IntentExecutorError::EmptyIntentError);
        }
        let all_committed_pubkeys = intent_bundle.get_all_committed_pubkeys();

        // Update tasks status to Pending
        {
            let update_status = CommitStatus::Pending;
            persist_status_update_by_message_set(
                persister,
                intent_bundle.id,
                &all_committed_pubkeys,
                update_status,
            );
        }

        if all_committed_pubkeys.is_empty() {
            // Build tasks for commit stage
            let commit_tasks = TaskBuilderImpl::commit_tasks(
                &self.task_info_fetcher,
                &intent_bundle,
                persister,
            )
            .await?;

            // Standalone actions executed in single stage
            let strategy = TaskStrategist::build_strategy(
                commit_tasks,
                &self.authority.pubkey(),
                persister,
            )?;
            return self
                .single_stage_execution_flow(intent_bundle, strategy, persister)
                .await;
        };

        // Build tasks for commit & finalize stages
        let (commit_tasks, finalize_tasks) = {
            let commit_tasks_fut = TaskBuilderImpl::commit_tasks(
                &self.task_info_fetcher,
                &intent_bundle,
                persister,
            );
            let finalize_tasks_fut = TaskBuilderImpl::finalize_tasks(
                &self.task_info_fetcher,
                &intent_bundle,
            );
            let (commit_tasks, finalize_tasks) =
                join(commit_tasks_fut, finalize_tasks_fut).await;

            (commit_tasks?, finalize_tasks?)
        };

        // Build execution strategy
        match TaskStrategist::build_execution_strategy(
            commit_tasks,
            finalize_tasks,
            &self.authority.pubkey(),
            persister,
        )? {
            StrategyExecutionMode::SingleStage(strategy) => {
                trace!("Single stage execution");
                self.single_stage_execution_flow(
                    intent_bundle,
                    strategy,
                    persister,
                )
                .await
            }
            StrategyExecutionMode::TwoStage {
                commit_stage,
                finalize_stage,
            } => {
                trace!("Two stage execution");
                self.two_stage_execution_flow(
                    &all_committed_pubkeys,
                    commit_stage,
                    finalize_stage,
                    persister,
                )
                .await
            }
        }
    }

    /// Starting execution from single stage
    pub async fn single_stage_execution_flow<P: IntentPersister>(
        &mut self,
        base_intent: ScheduledIntentBundle,
        transaction_strategy: TransactionStrategy,
        persister: &Option<P>,
    ) -> IntentExecutorResult<ExecutionOutput> {
        let started_at = self.started_at;
        let actions_timeout = self.actions_timeout;
        let time_left = || -> Option<Duration> {
            actions_timeout.checked_sub(started_at.elapsed())
        };

        let has_callbacks = transaction_strategy.has_actions_callbacks();
        let committed_pubkeys = base_intent.get_all_committed_pubkeys();

        let mut single_stage_executor =
            SingleStageExecutor::new(self, transaction_strategy);
        let res = if has_callbacks {
            if let Some(time_left) = time_left() {
                let execute_fut = single_stage_executor
                    .execute(&committed_pubkeys, persister);
                match timeout(time_left, execute_fut).await {
                    Ok(res) => res,
                    Err(_) => {
                        info!(
                            "Intent execution timed out, cleaning up actions"
                        );
                        single_stage_executor.execute_callbacks(false);
                        single_stage_executor
                            .execute(&committed_pubkeys, persister)
                            .await
                    }
                }
            } else {
                // Already tumed out
                // Handle timeout and continue execution
                single_stage_executor.execute_callbacks(false);
                single_stage_executor
                    .execute(&committed_pubkeys, persister)
                    .await
            }
        } else {
            single_stage_executor
                .execute(&committed_pubkeys, persister)
                .await
        };

        // Here we continue only IF the error is a limit-type execution error
        // We can recover that Error by splitting execution
        // in 2 stages - commit & finalize
        // Otherwise we return error
        let execution_err = match res {
            Err(IntentExecutorError::FailedToFinalizeError {
                err:
                    err
                    @ (TransactionStrategyExecutionError::CpiLimitError(_, _)
                        | TransactionStrategyExecutionError::LoadedAccountsDataSizeExceeded(
                            _,
                            _,
                        )),
                commit_signature: _,
                finalize_signature: _,
            }) if !committed_pubkeys.is_empty() => err,
            res => {
            single_stage_executor.execute_callbacks(res.is_ok());
                let transaction_strategy = single_stage_executor.transaction_strategy;
                self.junk.push(transaction_strategy);
                return res;
            }
        };

        // With actions, we can't predict num of CPIs
        // If we get here we will try to switch from Single stage to Two Stage commit
        // Note that this not necessarily will pass at the end due to the same reason
        let strategy = single_stage_executor.transaction_strategy;
        let (commit_strategy, finalize_strategy, cleanup) =
            self.handle_cpi_limit_error(strategy);
        self.junk.push(cleanup);
        self.patched_errors.push(execution_err);

        self.two_stage_execution_flow(
            &committed_pubkeys,
            commit_strategy,
            finalize_strategy,
            persister,
        )
        .await
    }

    pub async fn two_stage_execution_flow<P: IntentPersister>(
        &mut self,
        committed_pubkeys: &[Pubkey],
        commit_strategy: TransactionStrategy,
        finalize_strategy: TransactionStrategy,
        persister: &Option<P>,
    ) -> IntentExecutorResult<ExecutionOutput> {
        let started_at = self.started_at;
        let actions_timeout = self.actions_timeout;
        let time_left = || -> Option<Duration> {
            actions_timeout.checked_sub(started_at.elapsed())
        };

        let has_commit_callbacks = commit_strategy.has_actions_callbacks();
        let mut has_finalize_callbacks =
            finalize_strategy.has_actions_callbacks();
        let mut executor =
            TwoStageExecutor::new(self, commit_strategy, finalize_strategy);

        let commit_res = if has_commit_callbacks {
            if let Some(time_left) = time_left() {
                let fut = executor.commit(committed_pubkeys, persister);
                match timeout(time_left, fut).await {
                    Ok(res) => res,
                    Err(_) => {
                        // Timeout triggers all callbacks
                        has_finalize_callbacks = false;
                        executor.execute_callbacks(false);
                        executor.commit(committed_pubkeys, persister).await
                    }
                }
            } else {
                // Timeout triggers all callbacks
                has_finalize_callbacks = false;
                executor.execute_callbacks(false);
                executor.commit(committed_pubkeys, persister).await
            }
        } else {
            executor.commit(committed_pubkeys, persister).await
        };
        executor.execute_callbacks(commit_res.is_ok());
        let commit_signature = commit_res?;

        let mut finalize_executor = executor.done(commit_signature);
        let finalize_res = if has_finalize_callbacks {
            if let Some(time_left) = time_left() {
                let execute_fut = finalize_executor.finalize(persister);
                match timeout(time_left, execute_fut).await {
                    Ok(res) => res,
                    Err(_) => {
                        info!(
                            "Intent execution timed out, cleaning up actions"
                        );
                        finalize_executor.execute_callbacks(false);
                        finalize_executor.finalize(persister).await
                    }
                }
            } else {
                // Already tumed out
                // Handle timeout and continue execution
                finalize_executor.execute_callbacks(false);
                finalize_executor.finalize(persister).await
            }
        } else {
            finalize_executor.finalize(persister).await
        };
        finalize_executor.execute_callbacks(finalize_res.is_ok());

        let finalize_signature = finalize_res?;
        let finalized_stage = finalize_executor.done(finalize_signature);
        Ok(ExecutionOutput::TwoStage {
            commit_signature: finalized_stage.state.commit_signature,
            finalize_signature: finalized_stage.state.finalize_signature,
        })
    }

    /// Handles out of sync commit id error, fixes current strategy
    /// Returns strategy to be cleaned up
    /// TODO(edwin): TransactionStrategy -> CleanuoStrategy or something, naming it confusing for something that is cleaned up
    async fn handle_commit_id_error(
        &self,
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
        self.task_info_fetcher
            .reset(ResetType::Specific(committed_pubkeys));
        let commit_ids = self
            .task_info_fetcher
            .fetch_next_commit_ids(committed_pubkeys, min_context_slot)
            .await
            .map_err(TaskBuilderError::CommitTasksBuildError)?;

        // Here we find the broken tasks and reset them
        // Broken tasks are prepared incorrectly so they have to be cleaned up
        let mut to_cleanup = Vec::new();
        for task in commit_tasks {
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

        let old_alts = strategy.dummy_revaluate_alts(&self.authority.pubkey());
        Ok(TransactionStrategy {
            optimized_tasks: to_cleanup,
            lookup_tables_keys: old_alts,
        })
    }

    /// Handles actions error, stripping away actions
    /// Returns [`TransactionStrategy`] to be cleaned up
    // TODO(edwin): move to TransactionStrategy
    fn remove_actions(
        &self,
        strategy: &mut TransactionStrategy,
    ) -> TransactionStrategy {
        // Strip away actions
        let (optimized_tasks, action_tasks) = strategy
            .optimized_tasks
            .drain(..)
            .partition(|el| !matches!(el, BaseTaskImpl::BaseAction(_)));
        strategy.optimized_tasks = optimized_tasks;

        let old_alts = strategy.dummy_revaluate_alts(&self.authority.pubkey());

        TransactionStrategy {
            optimized_tasks: action_tasks,
            lookup_tables_keys: old_alts,
        }
    }

    /// Handle CPI limit error, splits single strategy flow into 2
    /// Returns Commit stage strategy, Finalize stage strategy and strategy to clean up
    fn handle_cpi_limit_error(
        &self,
        strategy: TransactionStrategy,
    ) -> (
        TransactionStrategy,
        TransactionStrategy,
        TransactionStrategy,
    ) {
        // We encountered error "Max instruction trace length exceeded"
        // All the tasks a prepared to be executed at this point
        // We attempt Two stages commit flow, need to split tasks up
        let (commit_stage_tasks, finalize_stage_tasks): (Vec<_>, Vec<_>) =
            strategy
                .optimized_tasks
                .into_iter()
                .partition(|el| matches!(el, BaseTaskImpl::Commit(_)));

        let commit_alt_pubkeys = if strategy.lookup_tables_keys.is_empty() {
            vec![]
        } else {
            TaskStrategist::collect_lookup_table_keys(
                &self.authority.pubkey(),
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
                &self.authority.pubkey(),
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
    fn handle_undelegation_error(
        &self,
        strategy: &mut TransactionStrategy,
    ) -> TransactionStrategy {
        let position = strategy
            .optimized_tasks
            .iter()
            .position(|el| matches!(el, BaseTaskImpl::Undelegate(_)));

        if let Some(position) = position {
            // Remove everything after undelegation including post undelegation actions
            let removed_task =
                strategy.optimized_tasks.drain(position..).collect();
            let old_alts =
                strategy.dummy_revaluate_alts(&self.authority.pubkey());
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

    /// Shared helper for sending transactions
    async fn send_prepared_message(
        &self,
        mut prepared_message: VersionedMessage,
    ) -> IntentExecutorResult<MagicBlockSendTransactionOutcome, InternalError>
    {
        let latest_blockhash = self.rpc_client.get_latest_blockhash().await?;
        match &mut prepared_message {
            VersionedMessage::V0(value) => {
                value.recent_blockhash = latest_blockhash;
            }
            VersionedMessage::Legacy(value) => {
                warn!("Legacy message not expected");
                value.recent_blockhash = latest_blockhash;
            }
        };

        let transaction = VersionedTransaction::try_new(
            prepared_message,
            &[&self.authority],
        )?;
        let result = self
            .rpc_client
            .send_transaction(
                &transaction,
                &MagicBlockSendTransactionConfig::ensure_committed(),
            )
            .await?;

        Ok(result)
    }

    /// Flushes result into presistor
    /// The result will be propagated down to callers
    fn persist_result<P: IntentPersister>(
        persistor: &P,
        result: &IntentExecutorResult<ExecutionOutput>,
        message_id: u64,
        pubkeys: &[Pubkey],
    ) {
        let update_status = match result {
            Ok(value) => {
                let signatures = match *value {
                    ExecutionOutput::SingleStage(signature) => {
                        CommitStatusSignatures {
                            commit_stage_signature: signature,
                            finalize_stage_signature: Some(signature),
                        }
                    }
                    ExecutionOutput::TwoStage {
                        commit_signature,
                        finalize_signature,
                    } => CommitStatusSignatures {
                        commit_stage_signature: commit_signature,
                        finalize_stage_signature: Some(finalize_signature),
                    },
                };
                let update_status = CommitStatus::Succeeded(signatures);
                persist_status_update_by_message_set(
                    persistor,
                    message_id,
                    pubkeys,
                    update_status,
                );

                if let Err(err) =
                    persistor.finalize_base_intent(message_id, *value)
                {
                    tracing::error!(error = ?err, "Failed to persist ExecutionOutput");
                }

                return;
            }
            Err(IntentExecutorError::EmptyIntentError)
            | Err(IntentExecutorError::FailedToFitError)
            | Err(IntentExecutorError::TaskBuilderError(_))
            | Err(IntentExecutorError::FailedCommitPreparationError(
                TransactionPreparatorError::SignerError(_),
            ))
            | Err(IntentExecutorError::FailedFinalizePreparationError(
                TransactionPreparatorError::SignerError(_),
            )) => Some(CommitStatus::Failed),
            Err(IntentExecutorError::FailedCommitPreparationError(
                TransactionPreparatorError::FailedToFitError,
            )) => Some(CommitStatus::PartOfTooLargeBundleToProcess),
            Err(IntentExecutorError::FailedCommitPreparationError(
                TransactionPreparatorError::DeliveryPreparationError(_),
            )) => {
                // Intermediate commit preparation progress recorded by DeliveryPreparator
                None
            }
            Err(IntentExecutorError::FailedToCommitError {
                err: _,
                signature,
            }) => {
                // Commit is a single TX, so if it fails, all of commited accounts marked FailedProcess
                let status_signature =
                    signature.map(|sig| CommitStatusSignatures {
                        commit_stage_signature: sig,
                        finalize_stage_signature: None,
                    });
                Some(CommitStatus::FailedProcess(status_signature))
            }
            Err(IntentExecutorError::FailedFinalizePreparationError(_)) => {
                // Not supported in persistor
                None
            }
            Err(IntentExecutorError::FailedToFinalizeError {
                err: _,
                commit_signature,
                finalize_signature,
            }) => {
                // Finalize is a single TX, so if it fails, all of commited accounts marked FailedFinalize
                let update_status =
                    if let Some(commit_signature) = commit_signature {
                        let signatures = CommitStatusSignatures {
                            commit_stage_signature: *commit_signature,
                            finalize_stage_signature: *finalize_signature,
                        };
                        CommitStatus::FailedFinalize(signatures)
                    } else {
                        CommitStatus::FailedProcess(None)
                    };

                Some(update_status)
            }
            Err(IntentExecutorError::SignerError(_)) => {
                Some(CommitStatus::Failed)
            }
        };

        if let Some(update_status) = update_status {
            persist_status_update_by_message_set(
                persistor,
                message_id,
                pubkeys,
                update_status,
            );
        }
    }

    pub async fn prepare_and_execute_strategy<P: IntentPersister>(
        &self,
        transaction_strategy: &mut TransactionStrategy,
        persister: &Option<P>,
    ) -> IntentExecutorResult<
        IntentExecutorResult<Signature, TransactionStrategyExecutionError>,
        TransactionPreparatorError,
    > {
        // Prepare message
        let prepared_message = self
            .transaction_preparator
            .prepare_for_strategy(
                &self.authority,
                transaction_strategy,
                persister,
            )
            .await?;

        // Execute strategy
        let execution_result = self
            .execute_message_with_retries(
                prepared_message,
                &transaction_strategy.optimized_tasks,
            )
            .await;

        Ok(execution_result)
    }

    async fn execute_message_with_retries(
        &self,
        prepared_message: VersionedMessage,
        tasks: &[BaseTaskImpl],
    ) -> IntentExecutorResult<Signature, TransactionStrategyExecutionError>
    {
        struct IntentErrorMapper<TxMap> {
            transaction_error_mapper: TxMap,
        }
        impl<TxMap> SendErrorMapper<InternalError> for IntentErrorMapper<TxMap>
        where
            TxMap: TransactionErrorMapper<
                ExecutionError = TransactionStrategyExecutionError,
            >,
        {
            type ExecutionError = TransactionStrategyExecutionError;
            fn map(&self, error: InternalError) -> Self::ExecutionError {
                match error {
                    InternalError::MagicBlockRpcClientError(err) => {
                        map_magicblock_client_error(
                            &self.transaction_error_mapper,
                            *err,
                        )
                    }
                    err => {
                        TransactionStrategyExecutionError::InternalError(err)
                    }
                }
            }

            fn decide_flow(
                err: &Self::ExecutionError,
            ) -> ControlFlow<(), Duration> {
                match err {
                    TransactionStrategyExecutionError::InternalError(
                        InternalError::MagicBlockRpcClientError(err),
                    ) => decide_rpc_error_flow(err),
                    _ => ControlFlow::Break(()),
                }
            }
        }

        const RETRY_FOR: Duration = Duration::from_secs(2 * 60);
        const MIN_ATTEMPTS: usize = 3;

        // Send with retries
        let send_error_mapper = IntentErrorMapper {
            transaction_error_mapper: IntentTransactionErrorMapper { tasks },
        };
        let attempt = || async {
            self.send_prepared_message(prepared_message.clone()).await
        };
        send_transaction_with_retries(
            attempt,
            send_error_mapper,
            |i, elapsed| !(elapsed < RETRY_FOR || i < MIN_ATTEMPTS),
        )
        .await
    }

    async fn intent_metrics(
        rpc_client: MagicblockRpcClient,
        execution_outcome: ExecutionOutput,
    ) {
        use solana_transaction_status_client_types::EncodedTransactionWithStatusMeta;
        fn extract_cu(tx: EncodedTransactionWithStatusMeta) -> Option<u64> {
            let cu = tx.meta?.compute_units_consumed;
            cu.into()
        }

        let config = RpcTransactionConfig {
            commitment: Some(rpc_client.commitment()),
            max_supported_transaction_version: Some(0),
            ..Default::default()
        };
        let cu_metrics = || async {
            match execution_outcome {
                ExecutionOutput::SingleStage(signature) => {
                    let tx = rpc_client
                        .get_transaction(&signature, Some(config))
                        .await?;
                    Ok::<_, MagicBlockRpcClientError>(extract_cu(
                        tx.transaction,
                    ))
                }
                ExecutionOutput::TwoStage {
                    commit_signature,
                    finalize_signature,
                } => {
                    let commit_tx = rpc_client
                        .get_transaction(&commit_signature, Some(config))
                        .await?;
                    let finalize_tx = rpc_client
                        .get_transaction(&finalize_signature, Some(config))
                        .await?;
                    let commit_cu = extract_cu(commit_tx.transaction);
                    let finalize_cu = extract_cu(finalize_tx.transaction);
                    let (Some(commit_cu), Some(finalize_cu)) =
                        (commit_cu, finalize_cu)
                    else {
                        return Ok(None);
                    };
                    Ok(Some(commit_cu + finalize_cu))
                }
            }
        };

        match cu_metrics().await {
            Ok(Some(cu)) => metrics::set_commmittor_intent_cu_usage(
                i64::try_from(cu).unwrap_or(i64::MAX),
            ),
            Err(err) => warn!(error = ?err, "Failed to fetch CUs for intent"),
            _ => {}
        }
    }
}

#[async_trait]
impl<T, C, A> IntentExecutor for IntentExecutorImpl<T, C, A>
where
    T: TransactionPreparator,
    C: TaskInfoFetcher,
    A: ActionsCallbackExecutor,
{
    /// Executes Message on Base layer
    /// Returns `ExecutionOutput` or an `Error`
    async fn execute<P: IntentPersister>(
        &mut self,
        base_intent: ScheduledIntentBundle,
        persister: Option<P>,
    ) -> IntentExecutionResult {
        self.started_at = Instant::now();
        let message_id = base_intent.id;
        let is_undelegate = base_intent.has_undelegate_intent();
        let pubkeys = base_intent.get_all_committed_pubkeys();

        let result = self.execute_inner(base_intent, &persister).await;
        if !pubkeys.is_empty() {
            // Reset TaskInfoFetcher, as cache could become invalid
            // NOTE: if undelegation was removed - we still reset
            // We assume its safe since all consecutive commits will fail
            if result.is_err() || is_undelegate {
                self.task_info_fetcher.reset(ResetType::Specific(&pubkeys));
            }

            // Write result of intent into Persister
            Self::persist_result(&persister, &result, message_id, &pubkeys);
        }

        // Gather metrics in separate task
        let result = result.inspect(|output| {
            tokio::spawn(Self::intent_metrics(
                self.rpc_client.clone(),
                *output,
            ));
        });

        IntentExecutionResult {
            inner: result,
            patched_errors: mem::take(&mut self.patched_errors),
        }
    }

    /// Cleanup after intent using junk
    async fn cleanup(mut self) -> Result<(), BufferExecutionError> {
        let cleanup_futs = self.junk.iter().map(|to_cleanup| {
            self.transaction_preparator.cleanup_for_strategy(
                &self.authority,
                &to_cleanup.optimized_tasks,
                &to_cleanup.lookup_tables_keys,
            )
        });

        try_join_all(cleanup_futs).await.map(|_| ())
    }
}
