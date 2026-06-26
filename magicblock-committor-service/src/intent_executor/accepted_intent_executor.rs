use std::time::{Duration, Instant};

use async_trait::async_trait;
use magicblock_core::traits::ActionsCallbackScheduler;
use magicblock_program::{
    magic_scheduled_base_intent::ScheduledIntentBundle,
    validator::validator_authority,
};
use solana_keypair::Keypair;
use solana_signer::Signer;
use tracing::trace;

use crate::{
    intent_executor::{
        cleanup_handle::CleanupHandle,
        error::{IntentExecutorError, IntentExecutorResult},
        strategy_executor::two_stage::Initialized,
        task_info_fetcher::{ResetType, TaskInfoFetcher},
        utils::{
            build_commit_finalize_tasks, execute_single_stage_flow,
            execute_two_stage_flow,
        },
        ExecutionOutput, IntentExecutionReport, IntentExecutionResult,
        IntentExecutor, IntentExecutorCtx,
    },
    outbox_client::OutboxClient,
    tasks::{
        task_builder::TasksBuilder,
        task_strategist::{
            StrategyExecutionMode, TaskStrategist, TransactionStrategy,
            TwoStageExecutionMode,
        },
        TaskBuilderImpl,
    },
    transaction_preparator::TransactionPreparator,
};

pub struct AcceptedIntentExecutor<T, F, A, O> {
    ctx: IntentExecutorCtx<T, F, A, O>,
    authority: Keypair,
    /// Intent execution started at
    pub started_at: Instant,
    /// Junk that needs to be cleaned up
    junk: Vec<TransactionStrategy>,
    /// Set to false on execution failure so cleanup only releases ALT
    /// reservations without closing buffer PDAs (see race condition note in
    /// intent_execution_engine)
    close_buffers: bool,
}

impl<T, F, A, O> AcceptedIntentExecutor<T, F, A, O>
where
    T: TransactionPreparator,
    F: TaskInfoFetcher,
    A: ActionsCallbackScheduler,
    O: OutboxClient,
    O::Error: Into<IntentExecutorError>,
{
    pub fn new(ctx: IntentExecutorCtx<T, F, A, O>) -> Self {
        let authority = validator_authority();
        Self {
            ctx,
            authority,
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
                &self.ctx.task_info_fetcher,
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
        let (commit_tasks, finalize_tasks) = build_commit_finalize_tasks(
            &intent_bundle,
            &self.ctx.task_info_fetcher,
        )
        .await?;

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
            StrategyExecutionMode::TwoStage(TwoStageExecutionMode {
                commit_stage,
                finalize_stage,
            }) => {
                trace!("Two stage execution");
                self.two_stage_execution_flow(
                    intent_bundle,
                    commit_stage,
                    finalize_stage,
                    execution_report,
                )
                .await
            }
        }
    }

    fn time_left(&self) -> Option<Duration> {
        self.ctx
            .actions_timeout
            .checked_sub(self.started_at.elapsed())
    }

    /// Starting execution from single stage
    pub async fn single_stage_execution_flow(
        &mut self,
        intent_bundle: ScheduledIntentBundle,
        transaction_strategy: TransactionStrategy,
        execution_report: &mut IntentExecutionReport,
    ) -> IntentExecutorResult<ExecutionOutput> {
        execute_single_stage_flow(
            &self.ctx,
            &self.authority,
            intent_bundle,
            transaction_strategy,
            execution_report,
            || self.time_left(),
        )
        .await
    }

    pub async fn two_stage_execution_flow(
        &mut self,
        intent_bundle: ScheduledIntentBundle,
        commit_strategy: TransactionStrategy,
        finalize_strategy: TransactionStrategy,
        execution_report: &mut IntentExecutionReport,
    ) -> IntentExecutorResult<ExecutionOutput> {
        let state = Initialized::new(commit_strategy, finalize_strategy);
        execute_two_stage_flow(
            &self.ctx,
            state,
            &self.authority,
            intent_bundle,
            execution_report,
            || self.time_left(),
        )
        .await
    }
}

#[async_trait]
impl<T, C, A, O> IntentExecutor<T> for AcceptedIntentExecutor<T, C, A, O>
where
    T: TransactionPreparator,
    C: TaskInfoFetcher,
    A: ActionsCallbackScheduler,
    O: OutboxClient,
    O::Error: Into<IntentExecutorError>,
{
    /// Executes Message on Base layer
    /// Returns `ExecutionOutput` or an `Error`
    async fn execute(
        mut self: Box<Self>,
        base_intent: ScheduledIntentBundle,
    ) -> (IntentExecutionResult, CleanupHandle<T>) {
        self.started_at = Instant::now();
        let is_undelegate = base_intent.has_undelegate_intent();
        let pubkeys = base_intent.get_all_committed_pubkeys();

        let mut execution_report = IntentExecutionReport::default();
        let result =
            self.execute_inner(base_intent, &mut execution_report).await;
        if !pubkeys.is_empty() {
            // Reset TaskInfoFetcher, as cache could become invalid
            // NOTE: if undelegation was removed - we still reset
            // We assume its safe since all consecutive commits will fail
            if result.is_err() || is_undelegate {
                self.ctx
                    .task_info_fetcher
                    .reset(ResetType::Specific(&pubkeys));
            }
        }

        // Gather metrics in separate task
        let intent_client = self.ctx.intent_client.clone();
        let result = result.inspect(|output| {
            let output_copy = *output;
            tokio::spawn(async move {
                intent_client.intent_metrics(output_copy).await
            });
        });

        self.close_buffers = result.is_ok();
        self.junk = execution_report.junk;
        let result = IntentExecutionResult {
            inner: result,
            patched_errors: execution_report.patched_errors,
            callbacks_report: execution_report.callbacks_report,
            #[cfg(feature = "dev-context-only-utils")]
            successful_transaction_strategies: execution_report
                .successful_transaction_strategies,
        };
        let cleanup_handle = CleanupHandle::new(
            self.authority,
            self.junk,
            self.close_buffers,
            self.ctx.transaction_preparator,
        );

        (result, cleanup_handle)
    }
}
