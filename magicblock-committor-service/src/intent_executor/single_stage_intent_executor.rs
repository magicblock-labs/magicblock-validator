use std::{
    ops::ControlFlow,
    time::{Duration, Instant},
};

use async_trait::async_trait;
use magicblock_core::traits::ActionsCallbackScheduler;
use magicblock_program::{
    magic_scheduled_base_intent::ScheduledIntentBundle,
    validator::validator_authority,
};
use solana_keypair::Keypair;
use solana_signature::Signature;
use solana_signer::Signer;

use crate::{
    intent_executor::{
        cleanup_handle::CleanupHandle,
        error::{IntentExecutorError, IntentExecutorResult},
        strategy_executor::utils::check_pending_signature,
        utils::{build_commit_finalize_tasks, execute_single_stage_flow},
        ExecutionOutput, IntentExecutionReport, IntentExecutionResult,
        IntentExecutor, IntentExecutorCtx,
    },
    outbox_client::OutboxClient,
    tasks::{
        task_info_fetcher::{ResetType, TaskInfoFetcher},
        task_strategist::{TaskStrategist, TransactionStrategy},
    },
    transaction_preparator::TransactionPreparator,
};

pub struct SingleStageIntentExecutor<T, F, A, O> {
    authority: Keypair,
    /// Signature of committing
    pending_signature: Signature,
    /// Intent Executor context
    ctx: IntentExecutorCtx<T, F, A, O>,

    /// Intent execution started at
    pub started_at: Instant,
    /// Junk that needs to be cleaned up
    junk: Vec<TransactionStrategy>,
    /// Set to false on execution failure so cleanup only releases ALT
    /// reservations without closing buffer PDAs (see race condition note in
    /// intent_execution_engine)
    close_buffers: bool,
}

impl<T, F, A, O> SingleStageIntentExecutor<T, F, A, O>
where
    T: TransactionPreparator,
    F: TaskInfoFetcher,
    A: ActionsCallbackScheduler,
    O: OutboxClient,
    O::Error: Into<IntentExecutorError>,
{
    pub fn new(
        ctx: IntentExecutorCtx<T, F, A, O>,
        pending_signature: Signature,
    ) -> Self {
        let authority = validator_authority();
        Self {
            authority,
            pending_signature,
            ctx,

            started_at: Instant::now(),
            junk: vec![],
            close_buffers: true,
        }
    }

    fn time_left(&self) -> Option<Duration> {
        self.ctx
            .actions_timeout
            .checked_sub(self.started_at.elapsed())
    }

    pub async fn execute_inner(
        &mut self,
        intent_bundle: ScheduledIntentBundle,
        execution_report: &mut IntentExecutionReport,
    ) -> IntentExecutorResult<ExecutionOutput> {
        let flow = check_pending_signature(
            &self.ctx.intent_client,
            &self.pending_signature,
        )
        .await?;
        match flow {
            // Intent was executed - return
            ControlFlow::Break(()) => {
                Ok(ExecutionOutput::SingleStage(self.pending_signature))
            }
            ControlFlow::Continue(()) => {
                // It we're here so previous run determined this should be single stage
                let (commit_tasks, finalize_tasks) =
                    build_commit_finalize_tasks(
                        &intent_bundle,
                        &self.ctx.task_info_fetcher,
                    )
                    .await?;

                let single_stage_tasks =
                    [commit_tasks, finalize_tasks].concat();
                let transaction_strategy = TaskStrategist::build_strategy(
                    single_stage_tasks,
                    &self.authority.pubkey(),
                )?;

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
        }
    }
}

#[async_trait]
impl<T, F, A, O> IntentExecutor<T> for SingleStageIntentExecutor<T, F, A, O>
where
    T: TransactionPreparator,
    F: TaskInfoFetcher,
    A: ActionsCallbackScheduler,
    O: OutboxClient,
    O::Error: Into<IntentExecutorError>,
{
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
