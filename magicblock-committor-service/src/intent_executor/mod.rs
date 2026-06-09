pub mod error;
pub mod intent_execution_client;
pub(crate) mod intent_executor_factory;
pub mod intent_executor_gateway;
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
    outbox_intent_bundles::OutboxIntentBundle, validator::validator_authority,
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
    outbox_client::OutboxClient,
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
    async fn execute(
        &mut self,
        base_intent: OutboxIntentBundle,
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

pub struct IntentExecutorImpl<T, F, A, O> {
    authority: Keypair,
    intent_client: IntentExecutionClient,
    transaction_preparator: T,
    task_info_fetcher: Arc<CacheTaskInfoFetcher<F>>,
    actions_callback_executor: A,
    outbox_client: Arc<O>,
    /// Timeout for Intent's actions
    actions_timeout: Duration,

    /// Intent execution started at
    pub started_at: Instant,
    /// Junk that needs to be cleaned up
    junk: Vec<TransactionStrategy>,
    /// Set to false on execution failure so cleanup only releases ALT
    /// reservations without closing buffer PDAs (see race condition note in
    /// intent_execution_engine)
    close_buffers: bool,
}

impl<T, F, A, O> IntentExecutorImpl<T, F, A, O>
where
    T: TransactionPreparator,
    F: TaskInfoFetcher,
    A: ActionsCallbackScheduler,
    O: OutboxClient,
{
    pub fn new(
        rpc_client: MagicblockRpcClient,
        transaction_preparator: T,
        task_info_fetcher: Arc<CacheTaskInfoFetcher<F>>,
        outbox_client: Arc<O>,
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
            outbox_client,
            actions_callback_executor,
            actions_timeout,

            started_at: Instant::now(),
            junk: vec![],
            close_buffers: true,
        }
    }

    async fn execute_inner(
        &mut self,
        intent_bundle: ScheduledIntentBundle,
        execution_report: &mut IntentExecutionReport,
    ) -> IntentExecutorResult<ExecutionOutput> {
        if intent_bundle.is_empty() {
            return Err(IntentExecutorError::EmptyIntentError);
        }
        let all_committed_pubkeys = intent_bundle.get_all_committed_pubkeys();

        if all_committed_pubkeys.is_empty() {
            // Build tasks for commit stage
            // TODO (snawaz): it's actually MagicBaseIntent::BaseActions scenario, not Commit
            // scenario, so the related code needs little bit of refactoring and proper renaming.
            let commit_tasks = TaskBuilderImpl::commit_tasks(
                &self.task_info_fetcher,
                &intent_bundle,
            )
            .await?;

            // Standalone actions executed in single stage
            let mut strategy = TaskStrategist::build_strategy(
                commit_tasks,
                &self.authority.pubkey(),
            )?;
            strategy.standalone_action_nonce = Some(intent_bundle.id);
            return self
                .single_stage_execution_flow(
                    intent_bundle,
                    strategy,
                    execution_report,
                )
                .await;
        };

        // Build tasks for commit & finalize stages
        let (commit_tasks, finalize_tasks) = {
            let commit_tasks_fut = TaskBuilderImpl::commit_tasks(
                &self.task_info_fetcher,
                &intent_bundle,
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
        )? {
            StrategyExecutionMode::SingleStage(strategy) => {
                trace!("Single stage execution");
                self.single_stage_execution_flow(
                    intent_bundle,
                    strategy,
                    execution_report,
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
                )
                .await
            }
        }
    }

    fn time_left(&self) -> Option<Duration> {
        self.actions_timeout.checked_sub(self.started_at.elapsed())
    }

    /// Starting execution from single stage
    pub async fn single_stage_execution_flow(
        &mut self,
        base_intent: ScheduledIntentBundle,
        transaction_strategy: TransactionStrategy,
        execution_report: &mut IntentExecutionReport,
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
        )
        .await;

        // Here we continue only IF the error is a limit-type execution error
        // We can recover that Error by splitting execution
        // in 2 stages - commit & finalize
        // Otherwise we return error
        let execution_err = match res {
            Err(IntentExecutorError::FailedToFinalizeError {
                err,
                commit_signature: _,
                finalize_signature: _,
            }) if !committed_pubkeys.is_empty()
                && err.is_single_stage_split_limit_error() =>
            {
                err
            }
            res => {
                let signature = res.as_ref().ok().copied();
                single_stage_executor
                    .execute_callbacks(signature, res.as_ref().map(|_| ()));
                let transaction_strategy =
                    single_stage_executor.consume_strategy();
                execution_report.dispose(transaction_strategy);
                return res.map(ExecutionOutput::SingleStage);
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
        )
        .await
    }

    pub async fn two_stage_execution_flow(
        &mut self,
        committed_pubkeys: &[Pubkey],
        commit_strategy: TransactionStrategy,
        finalize_strategy: TransactionStrategy,
        execution_report: &mut IntentExecutionReport,
    ) -> IntentExecutorResult<ExecutionOutput> {
        let mut executor = TwoStageExecutor::new(
            self.authority.insecure_clone(),
            commit_strategy,
            finalize_strategy,
            self.intent_client.clone(),
            self.outbox_client.clone(),
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
        )
        .await?;

        let mut finalize_executor = executor.done(commit_signature);
        let finalize_signature = execute_with_timeout(
            self.time_left(),
            FinalizeStage {
                inner: &mut finalize_executor,
                transaction_preparator: &self.transaction_preparator,
            },
        )
        .await?;

        let finalized_stage = finalize_executor.done(finalize_signature);
        Ok(ExecutionOutput::TwoStage {
            commit_signature: finalized_stage.commit_signature,
            finalize_signature: finalized_stage.finalize_signature,
        })
    }
}

#[async_trait]
impl<T, C, A, O> IntentExecutor for IntentExecutorImpl<T, C, A, O>
where
    T: TransactionPreparator,
    C: TaskInfoFetcher,
    A: ActionsCallbackScheduler,
    O: OutboxClient,
{
    /// Executes Message on Base layer
    /// Returns `ExecutionOutput` or an `Error`
    async fn execute(
        &mut self,
        base_intent: OutboxIntentBundle,
    ) -> IntentExecutionResult {
        self.started_at = Instant::now();
        let is_undelegate = base_intent.has_undelegate_intent();
        let pubkeys = base_intent.get_all_committed_pubkeys();

        let mut execution_report = IntentExecutionReport::default();
        let result = self
            .execute_inner(base_intent.inner, &mut execution_report)
            .await;
        if !pubkeys.is_empty() {
            // Reset TaskInfoFetcher, as cache could become invalid
            // NOTE: if undelegation was removed - we still reset
            // We assume its safe since all consecutive commits will fail
            if result.is_err() || is_undelegate {
                self.task_info_fetcher.reset(ResetType::Specific(&pubkeys));
            }
        }

        // Gather metrics in separate task
        let intent_client = self.intent_client.clone();
        let result = result.inspect(|output| {
            let output_copy = *output;
            tokio::spawn(async move {
                intent_client.intent_metrics(output_copy).await
            });
        });

        self.close_buffers = result.is_ok();
        self.junk = execution_report.junk;
        IntentExecutionResult {
            inner: result,
            patched_errors: execution_report.patched_errors,
            callbacks_report: execution_report.callbacks_report,
        }
    }

    async fn cleanup(mut self) -> Result<(), BufferExecutionError> {
        let close_buffers = self.close_buffers;
        let cleanup_futs = self.junk.iter().map(|to_cleanup| {
            self.transaction_preparator.cleanup_for_strategy(
                &self.authority,
                &to_cleanup.optimized_tasks,
                &to_cleanup.lookup_tables_keys,
                close_buffers,
            )
        });

        try_join_all(cleanup_futs).await.map(|_| ())
    }
}
