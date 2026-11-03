use std::time::{Duration, Instant};

use async_trait::async_trait;
use mbv_core::traits::ActionsCallbackScheduler;
use program::magic_scheduled_base_intent::ScheduledIntentBundle;
use solana_keypair::Keypair;
use solana_signer::Signer;

use crate::{
    intent_executor::{
        ExecutionOutput, IntentExecutionReport, IntentExecutionResult, IntentExecutor,
        IntentExecutorCtx,
        cleanup_handle::CleanupHandle,
        error::{IntentExecutorError, IntentExecutorResult},
        strategy_executor::{transaction::TransactionExecutor, utils::requires_uniqueness_nonce},
        utils::{build_commit_finalize_tasks, report_and_close_intent},
    },
    outbox::{OutboxClient, ScheduledBaseIntentMeta},
    tasks::{
        TaskBuilderImpl,
        task_builder::TasksBuilder,
        task_info_fetcher::{ResetType, TaskInfoFetcher},
        task_strategist::{StrategyExecutionMode, TaskStrategist},
    },
    transaction_preparator::TransactionPreparator,
};

pub struct AcceptedIntentExecutor<T, F, A, O> {
    ctx: IntentExecutorCtx<T, F, A, O>,
    authority: Keypair,
    /// Timeout for Intent's actions
    pub actions_timeout: Duration,
    /// Intent execution started at
    pub started_at: Instant,
}

impl<T, F, A, O> AcceptedIntentExecutor<T, F, A, O>
where
    T: TransactionPreparator,
    F: TaskInfoFetcher,
    A: ActionsCallbackScheduler,
    O: OutboxClient,
    O::Error: Into<IntentExecutorError>,
{
    pub fn new(ctx: IntentExecutorCtx<T, F, A, O>, actions_timeout: Duration) -> Self {
        let authority = ctx.authority.insecure_clone();
        Self {
            ctx,
            authority,
            actions_timeout,
            started_at: Instant::now(),
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
            let commit_tasks =
                TaskBuilderImpl::commit_tasks(&self.ctx.task_info_fetcher, &intent_bundle).await?;

            // Standalone actions executed in single stage
            let strategy = TaskStrategist::build_strategy(
                commit_tasks,
                &self.authority.pubkey(),
                Some(intent_bundle.intent_id),
            )?;
            return TransactionExecutor::new(
                &self.ctx,
                execution_report,
                intent_bundle.intent_id,
                &all_committed_pubkeys,
                StrategyExecutionMode::SingleStage(strategy),
            )
            .execute(|| self.time_left())
            .await;
        };

        // Build tasks for commit & finalize stages
        let (commit_tasks, finalize_tasks) =
            build_commit_finalize_tasks(&intent_bundle, &self.ctx.task_info_fetcher).await?;

        let uniqueness_nonce =
            requires_uniqueness_nonce(&commit_tasks).then_some(intent_bundle.intent_id);

        // Build execution strategy
        let strategy = TaskStrategist::build_execution_strategy(
            commit_tasks,
            finalize_tasks,
            &self.authority.pubkey(),
            uniqueness_nonce,
        )?;
        TransactionExecutor::new(
            &self.ctx,
            execution_report,
            intent_bundle.intent_id,
            &all_committed_pubkeys,
            strategy,
        )
        .execute(|| self.time_left())
        .await
    }

    fn time_left(&self) -> Option<Duration> {
        self.actions_timeout.checked_sub(self.started_at.elapsed())
    }
}

#[async_trait]
impl<T, F, A, O> IntentExecutor<T> for AcceptedIntentExecutor<T, F, A, O>
where
    T: TransactionPreparator,
    F: TaskInfoFetcher,
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
        let meta = ScheduledBaseIntentMeta::new(&base_intent);
        let pubkeys = base_intent.get_all_committed_pubkeys();
        let undelegated_pubkeys = base_intent.get_undelegated_pubkeys();

        let mut execution_report = IntentExecutionReport::default();
        let result = {
            let result = self.execute_inner(base_intent, &mut execution_report).await;
            report_and_close_intent(
                result,
                meta,
                &mut execution_report,
                self.ctx.outbox_client.as_ref(),
            )
            .await
        };
        if !pubkeys.is_empty() {
            if result.is_err() {
                // We can't know what landed on chain, resync everything
                self.ctx.task_info_fetcher.reset(ResetType::Specific(&pubkeys));
            } else if !undelegated_pubkeys.is_empty() {
                // Only undelegated accounts' nonces become stale. Keep the
                // rest cached: a chain re-fetch can race the just-landed
                // finalize and reuse a nonce (buffer PDA collision).
                self.ctx.task_info_fetcher.reset(ResetType::Specific(&undelegated_pubkeys));
            }
        }

        // Gather metrics in separate task
        let intent_client = self.ctx.intent_client.clone();
        let result = result.inspect(|output| {
            let output_copy = *output;
            tokio::spawn(async move { intent_client.intent_metrics(output_copy).await });
        });

        let close_buffers = result.is_ok();
        let junk = execution_report.junk;
        let result = IntentExecutionResult {
            inner: result,
            patched_errors: execution_report.patched_errors,
            callbacks_report: execution_report.callbacks_report,
        };
        let cleanup_handle = CleanupHandle::new(
            self.authority,
            junk,
            close_buffers,
            self.ctx.transaction_preparator,
        );

        (result, cleanup_handle)
    }
}
