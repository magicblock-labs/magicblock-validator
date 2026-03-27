pub mod error;
pub mod intent_execution_client;
pub(crate) mod intent_executor_factory;
pub mod single_stage_executor;
pub mod task_info_fetcher;
pub mod two_stage_executor;
pub mod utils;

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use async_trait::async_trait;
use futures_util::future::{join, try_join_all};
use magicblock_core::traits::{
    ActionsCallbackScheduler, CallbackScheduleError,
};
use magicblock_metrics::metrics;
use magicblock_program::{
    magic_scheduled_base_intent::ScheduledIntentBundle,
    validator::validator_authority,
};
use magicblock_rpc_client::MagicblockRpcClient;
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_signature::Signature;
use solana_signer::Signer;
use tracing::trace;

use crate::{
    intent_executor::{
        error::{
            IntentExecutorError, IntentExecutorResult,
            TransactionStrategyExecutionError,
        },
        intent_execution_client::IntentExecutionClient,
        single_stage_executor::SingleStageExecutor,
        task_info_fetcher::{CacheTaskInfoFetcher, ResetType, TaskInfoFetcher},
        two_stage_executor::TwoStageExecutor,
        utils::{
            execute_with_timeout, handle_cpi_limit_error, CommitStage,
            FinalizeStage, SingleStage,
        },
    },
    persist::{CommitStatus, CommitStatusSignatures, IntentPersister},
    tasks::{
        task_builder::{TaskBuilderImpl, TasksBuilder},
        task_strategist::{
            StrategyExecutionMode, TaskStrategist, TransactionStrategy,
        },
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

#[derive(Debug)]
pub struct IntentExecutionResult {
    /// Final result of Intent Execution
    pub inner: IntentExecutorResult<ExecutionOutput>,
    /// Errors patched along the way
    pub patched_errors: Vec<TransactionStrategyExecutionError>,
    /// Callbacks result
    pub callbacks_report: Vec<Result<Signature, CallbackScheduleError>>,
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

#[derive(Default)]
pub struct IntentExecutionReport {
    /// Junk that needs to be cleaned up
    junk: Vec<TransactionStrategy>,
    /// Errors we patched trying to recover intent
    patched_errors: Vec<TransactionStrategyExecutionError>,
    /// Report of scheduled callbacks
    callbacks_report: Vec<Result<Signature, CallbackScheduleError>>,
}

impl IntentExecutionReport {
    pub fn dispose(&mut self, value: TransactionStrategy) {
        self.junk.push(value);
    }

    pub fn add_patched_error(
        &mut self,
        value: TransactionStrategyExecutionError,
    ) {
        self.patched_errors.push(value);
    }

    pub fn patched_errors(&self) -> &Vec<TransactionStrategyExecutionError> {
        &self.patched_errors
    }

    pub fn add_callback_report(
        &mut self,
        values: impl IntoIterator<Item = Result<Signature, CallbackScheduleError>>,
    ) {
        self.callbacks_report.extend(values);
    }

    pub fn junk(&self) -> &Vec<TransactionStrategy> {
        &self.junk
    }
}

pub struct IntentExecutorImpl<T, F, A> {
    authority: Keypair,
    intent_client: IntentExecutionClient,
    transaction_preparator: T,
    task_info_fetcher: Arc<CacheTaskInfoFetcher<F>>,
    actions_callback_executor: A,
    /// Timeout for Intent's actions
    actions_timeout: Duration,

    /// Intent execution started at
    pub started_at: Instant,
    /// Junk that needs to be cleaned up
    junk: Vec<TransactionStrategy>,
}

impl<T, F, A> IntentExecutorImpl<T, F, A>
where
    T: TransactionPreparator,
    F: TaskInfoFetcher,
    A: ActionsCallbackScheduler,
{
    pub fn new(
        rpc_client: MagicblockRpcClient,
        transaction_preparator: T,
        task_info_fetcher: Arc<CacheTaskInfoFetcher<F>>,
        actions_callback_executor: A,
        actions_timeout: Duration,
    ) -> Self {
        let authority = validator_authority();
        let intent_client = IntentExecutionClient::new(rpc_client);
        Self {
            authority,
            intent_client,
            transaction_preparator,
            task_info_fetcher,
            actions_callback_executor,
            actions_timeout,

            started_at: Instant::now(),
            junk: vec![],
        }
    }

    async fn execute_inner<P: IntentPersister>(
        &mut self,
        intent_bundle: ScheduledIntentBundle,
        execution_report: &mut IntentExecutionReport,
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
            // TODO (snawaz): it's actually MagicBaseIntent::BaseActions scenario, not Commit
            // scenario, so the related code needs little bit of refactoring and proper renaming.
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
                .single_stage_execution_flow(
                    intent_bundle,
                    strategy,
                    execution_report,
                    persister,
                )
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
                    execution_report,
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
                    execution_report,
                    persister,
                )
                .await
            }
        }
    }

    fn time_left(&self) -> Option<Duration> {
        self.actions_timeout.checked_sub(self.started_at.elapsed())
    }

    /// Starting execution from single stage
    pub async fn single_stage_execution_flow<P: IntentPersister>(
        &mut self,
        base_intent: ScheduledIntentBundle,
        transaction_strategy: TransactionStrategy,
        execution_report: &mut IntentExecutionReport,
        persister: &Option<P>,
    ) -> IntentExecutorResult<ExecutionOutput> {
        let committed_pubkeys = base_intent.get_all_committed_pubkeys();

        let mut single_stage_executor = SingleStageExecutor::new(
            self.authority.insecure_clone(),
            self.intent_client.clone(),
            self.task_info_fetcher.clone(),
            transaction_strategy,
            self.actions_callback_executor.clone(),
            execution_report,
        );
        let res = execute_with_timeout(
            self.time_left(),
            SingleStage {
                inner: &mut single_stage_executor,
                transaction_preparator: &self.transaction_preparator,
                committed_pubkeys: &committed_pubkeys,
            },
            persister,
        )
        .await;

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
            single_stage_executor.execute_callbacks(res.as_ref().map(|_| ()));
                let transaction_strategy = single_stage_executor.consume_strategy();
                execution_report.dispose(transaction_strategy);
                return res;
            }
        };

        // With actions, we can't predict num of CPIs
        // If we get here we will try to switch from Single stage to Two Stage commit
        // Note that this not necessarily will pass at the end due to the same reason
        let strategy = single_stage_executor.consume_strategy();
        let (commit_strategy, finalize_strategy, cleanup) =
            handle_cpi_limit_error(&self.authority.pubkey(), strategy);
        execution_report.dispose(cleanup);
        execution_report.add_patched_error(execution_err);

        self.two_stage_execution_flow(
            &committed_pubkeys,
            commit_strategy,
            finalize_strategy,
            execution_report,
            persister,
        )
        .await
    }

    pub async fn two_stage_execution_flow<P: IntentPersister>(
        &mut self,
        committed_pubkeys: &[Pubkey],
        commit_strategy: TransactionStrategy,
        finalize_strategy: TransactionStrategy,
        execution_report: &mut IntentExecutionReport,
        persister: &Option<P>,
    ) -> IntentExecutorResult<ExecutionOutput> {
        let mut executor = TwoStageExecutor::new(
            self.authority.insecure_clone(),
            commit_strategy,
            finalize_strategy,
            self.intent_client.clone(),
            self.actions_callback_executor.clone(),
            execution_report,
        );

        let commit_signature = execute_with_timeout(
            self.time_left(),
            CommitStage {
                inner: &mut executor,
                transaction_preparator: &self.transaction_preparator,
                task_info_fetcher: &self.task_info_fetcher,
                committed_pubkeys,
            },
            persister,
        )
        .await?;

        let mut finalize_executor = executor.done(commit_signature);
        let finalize_signature = execute_with_timeout(
            self.time_left(),
            FinalizeStage {
                inner: &mut finalize_executor,
                transaction_preparator: &self.transaction_preparator,
            },
            persister,
        )
        .await?;

        let finalized_stage = finalize_executor.done(finalize_signature);
        Ok(ExecutionOutput::TwoStage {
            commit_signature: finalized_stage.commit_signature,
            finalize_signature: finalized_stage.finalize_signature,
        })
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
}

#[async_trait]
impl<T, C, A> IntentExecutor for IntentExecutorImpl<T, C, A>
where
    T: TransactionPreparator,
    C: TaskInfoFetcher,
    A: ActionsCallbackScheduler,
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

        let mut execution_report = IntentExecutionReport::default();
        let result = self
            .execute_inner(base_intent, &mut execution_report, &persister)
            .await;
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
        let intent_client = self.intent_client.clone();
        let result = result.inspect(|output| {
            let output_copy = *output;
            tokio::spawn(async move {
                intent_client.intent_metrics(output_copy).await
            });
        });

        self.junk = execution_report.junk;
        IntentExecutionResult {
            inner: result,
            patched_errors: execution_report.patched_errors,
            callbacks_report: execution_report.callbacks_report,
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
