use std::{
    ops::ControlFlow,
    time::{Duration, Instant},
};

use async_trait::async_trait;
use magicblock_core::traits::ActionsCallbackScheduler;
use magicblock_program::{
    magic_scheduled_base_intent::ScheduledIntentBundle,
    outbox::TwoStageProgress, validator::validator_authority,
};
use solana_keypair::Keypair;
use solana_signature::Signature;
use solana_signer::Signer;

use crate::{
    intent_executor::{
        cleanup_handle::CleanupHandle,
        error::{IntentExecutorError, IntentExecutorResult},
        strategy_executor::{
            two_stage::{Committed, Initialized, TwoStageStrategyExecutor},
            utils::{
                check_pending_signature, execute_with_timeout, FinalizeStage,
            },
        },
        task_info_fetcher::{ResetType, TaskInfoFetcher},
        utils::{build_commit_finalize_tasks, execute_two_stage_flow},
        ExecutionOutput, IntentExecutionReport, IntentExecutionResult,
        IntentExecutor, IntentExecutorCtx,
    },
    outbox_client::OutboxClient,
    tasks::{
        task_builder::{TaskBuilderImpl, TasksBuilder},
        task_strategist::{
            TaskStrategist, TransactionStrategy, TwoStageExecutionMode,
        },
    },
    transaction_preparator::TransactionPreparator,
};

pub struct TwoStageIntentExecutor<T, F, A, O> {
    authority: Keypair,
    /// Current stage of TwoStage execution flow
    stage: TwoStageProgress,
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

impl<T, F, A, O> TwoStageIntentExecutor<T, F, A, O>
where
    T: TransactionPreparator,
    F: TaskInfoFetcher,
    A: ActionsCallbackScheduler,
    O: OutboxClient,
    O::Error: Into<IntentExecutorError>,
{
    pub fn new(
        ctx: IntentExecutorCtx<T, F, A, O>,
        stage: TwoStageProgress,
    ) -> Self {
        let authority = validator_authority();
        Self {
            authority,
            stage,
            ctx,

            // TODO(edwin): deduce started_at properly
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

    /// Picks up execution from commit stage signature
    async fn execute_committing_intent(
        &mut self,
        intent_bundle: ScheduledIntentBundle,
        execution_report: &mut IntentExecutionReport,
    ) -> IntentExecutorResult<ExecutionOutput> {
        // This stage was chosen prior so we build tasks for it
        // Build tasks for commit & finalize stages
        let (commit_tasks, finalize_tasks) = build_commit_finalize_tasks(
            &intent_bundle,
            &self.ctx.task_info_fetcher,
        )
        .await?;

        // As strategy was chosen build two stage
        let TwoStageExecutionMode {
            commit_stage,
            finalize_stage,
        } = TaskStrategist::build_two_stage(
            commit_tasks,
            finalize_tasks,
            &self.authority.pubkey(),
        )?;

        let state = Initialized::new(commit_stage, finalize_stage);
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

    /// Picks up execution from pending finalize signature
    async fn execute_finalizing_intent(
        &mut self,
        intent: ScheduledIntentBundle,
        commit_signature: Signature,
        execution_report: &mut IntentExecutionReport,
    ) -> IntentExecutorResult<ExecutionOutput> {
        // Commit succeeded so we skip those tasks all together
        let finalize_tasks = TaskBuilderImpl::finalize_tasks(
            &self.ctx.task_info_fetcher,
            &intent,
        )
        .await?;

        // Build strategy for finalize tasks
        let finalize_strategy = TaskStrategist::build_strategy(
            finalize_tasks,
            &self.authority.pubkey(),
        )?;

        let committed_state =
            Committed::new(commit_signature, finalize_strategy);
        let mut finalize_strategy_executor =
            TwoStageStrategyExecutor::committed(
                committed_state,
                self.authority.insecure_clone(),
                intent.id,
                self.ctx.intent_client.clone(),
                self.ctx.outbox_client.clone(),
                self.ctx.actions_callback_executor.clone(),
                execution_report,
            );

        let finalize_signature = execute_with_timeout(
            self.time_left(),
            FinalizeStage {
                inner: &mut finalize_strategy_executor,
                transaction_preparator: &self.ctx.transaction_preparator,
            },
        )
        .await?;

        let finalized_stage =
            finalize_strategy_executor.done(finalize_signature);
        Ok(ExecutionOutput::TwoStage {
            commit_signature: finalized_stage.commit_signature,
            finalize_signature: finalized_stage.finalize_signature,
        })
    }

    async fn execute_inner(
        &mut self,
        intent: ScheduledIntentBundle,
        execution_report: &mut IntentExecutionReport,
    ) -> IntentExecutorResult<ExecutionOutput> {
        let pending_signature = self.stage.pending_signature();
        let flow =
            check_pending_signature(&self.ctx.intent_client, pending_signature)
                .await?;

        match (&self.stage, flow) {
            // Signature wasn't confirmed - need to reexecute from commit
            (TwoStageProgress::Committing(_), ControlFlow::Continue(())) => {
                self.execute_committing_intent(intent, execution_report)
                    .await
            }
            // Signature confirmed - commit was executed, finalizing...
            (TwoStageProgress::Committing(commit), ControlFlow::Break(())) => {
                self.execute_finalizing_intent(
                    intent,
                    *commit,
                    execution_report,
                )
                .await
            }
            // Finalize didn't occur - execute
            (
                TwoStageProgress::Finalizing { commit, .. },
                ControlFlow::Continue(()),
            ) => {
                self.execute_finalizing_intent(
                    intent,
                    *commit,
                    execution_report,
                )
                .await
            }
            // Finalize was executed - return
            (
                TwoStageProgress::Finalizing { commit, finalize },
                ControlFlow::Break(()),
            ) => Ok(ExecutionOutput::TwoStage {
                commit_signature: *commit,
                finalize_signature: *finalize,
            }),
        }
    }
}

#[async_trait]
impl<T, F, A, O> IntentExecutor<T> for TwoStageIntentExecutor<T, F, A, O>
where
    T: TransactionPreparator,
    F: TaskInfoFetcher,
    A: ActionsCallbackScheduler,
    O: OutboxClient,
    O::Error: Into<IntentExecutorError>,
{
    async fn execute(
        mut self: Box<Self>,
        intent: ScheduledIntentBundle,
    ) -> (IntentExecutionResult, CleanupHandle<T>) {
        // TODO(edwin): see if can be extracted into single utils
        // Duplicates AcceptedIntentExecutor::execute
        let is_undelegate = intent.has_undelegate_intent();
        // TODO(edwin): should validate non emptiness of pubkeys? Shouldn't be possible tho
        let pubkeys = intent.get_all_committed_pubkeys();

        let mut execution_report = IntentExecutionReport::default();
        let result = self.execute_inner(intent, &mut execution_report).await;

        if is_undelegate {
            self.ctx
                .task_info_fetcher
                .reset(ResetType::Specific(&pubkeys));
        }
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
