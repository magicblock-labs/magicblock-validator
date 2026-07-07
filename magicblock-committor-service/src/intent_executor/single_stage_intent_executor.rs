use std::time::{Duration, Instant};

use async_trait::async_trait;
use magicblock_core::traits::ActionsCallbackScheduler;
use magicblock_program::{
    magic_scheduled_base_intent::ScheduledIntentBundle,
    outbox::PendingTransaction, validator::validator_authority,
};
use solana_keypair::Keypair;
use solana_signer::Signer;

use crate::{
    intent_executor::{
        cleanup_handle::CleanupHandle,
        error::{IntentExecutorError, IntentExecutorResult},
        strategy_executor::utils::resolve_pending_signature,
        utils::{build_commit_finalize_tasks, execute_single_stage_flow},
        ExecutionOutput, IntentExecutionReport, IntentExecutionResult,
        IntentExecutor, IntentExecutorCtx,
    },
    outbox::{OutboxClient, ScheduledBaseIntentMeta},
    tasks::{
        task_info_fetcher::{ResetType, TaskInfoFetcher},
        task_strategist::TaskStrategist,
    },
    transaction_preparator::TransactionPreparator,
};

pub struct SingleStageIntentExecutor<T, F, A, O> {
    authority: Keypair,
    /// data of pending stage
    pending_transaction: PendingTransaction,
    /// Intent Executor context
    ctx: IntentExecutorCtx<T, F, A, O>,

    /// Timeout for Intent's actions
    pub actions_timeout: Duration,
    /// Intent execution started at
    pub started_at: Instant,
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
        actions_timeout: Duration,
        pending_transaction: PendingTransaction,
    ) -> Self {
        let authority = validator_authority();
        Self {
            authority,
            pending_transaction,
            ctx,

            actions_timeout,
            started_at: Instant::now(),
        }
    }

    fn time_left(&self) -> Option<Duration> {
        self.actions_timeout.checked_sub(self.started_at.elapsed())
    }

    pub async fn execute_inner(
        &mut self,
        intent_bundle: ScheduledIntentBundle,
        execution_report: &mut IntentExecutionReport,
    ) -> IntentExecutorResult<ExecutionOutput> {
        let succeeded = resolve_pending_signature(
            &self.ctx.intent_client,
            &self.pending_transaction,
        )
        .await?;
        match succeeded {
            // Intent was already executed on a previous run - notify the
            // outbox so it isn't left pending and rediscovered again.
            true => {
                let output = Ok(ExecutionOutput::SingleStage(
                    self.pending_transaction.signature,
                ));
                self.ctx
                    .outbox_client
                    .notify_commit_sent(
                        ScheduledBaseIntentMeta::new(&intent_bundle),
                        &output,
                        execution_report,
                    )
                    .await
                    .map_err(Into::into)?;

                output
            }
            false => {
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
        let pubkeys = base_intent.get_all_committed_pubkeys();
        let undelegated_pubkeys = base_intent.get_undelegated_pubkeys();

        let mut execution_report = IntentExecutionReport::default();
        let result =
            self.execute_inner(base_intent, &mut execution_report).await;
        if !pubkeys.is_empty() {
            if result.is_err() {
                // We can't know what landed on chain, resync everything
                self.ctx
                    .task_info_fetcher
                    .reset(ResetType::Specific(&pubkeys));
            } else if !undelegated_pubkeys.is_empty() {
                // Only undelegated accounts' nonces become stale. Keep the
                // rest cached: a chain re-fetch can race the just-landed
                // finalize and reuse a nonce (buffer PDA collision).
                self.ctx
                    .task_info_fetcher
                    .reset(ResetType::Specific(&undelegated_pubkeys));
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

        let close_buffers = result.is_ok();
        let junk = execution_report.junk;
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
            junk,
            close_buffers,
            self.ctx.transaction_preparator,
        );

        (result, cleanup_handle)
    }
}
