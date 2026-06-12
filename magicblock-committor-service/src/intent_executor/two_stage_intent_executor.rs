use std::time::Instant;

use async_trait::async_trait;
use magicblock_core::traits::ActionsCallbackScheduler;
use magicblock_program::{
    magic_scheduled_base_intent::ScheduledIntentBundle,
    outbox::TwoStageProgress, validator::validator_authority,
};
use solana_keypair::Keypair;
use solana_signature::Signature;

use crate::{
    intent_executor::{
        cleanup_handle::CleanupHandle,
        error::{IntentExecutorError, IntentExecutorResult},
        task_info_fetcher::TaskInfoFetcher,
        ExecutionOutput, IntentExecutionReport, IntentExecutionResult,
        IntentExecutor, IntentExecutorCtx,
    },
    outbox_client::OutboxClient,
    tasks::task_strategist::TransactionStrategy,
    transaction_preparator::TransactionPreparator,
};
use crate::intent_executor::task_info_fetcher::ResetType;

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

            started_at: Instant::now(),
            junk: vec![],
            close_buffers: true,
        }
    }

    async fn execute_committing_intent(
        &mut self,
        intent: ScheduledIntentBundle,
        pending_signature: Signature,
        execution_report: &mut IntentExecutionReport,
    ) -> IntentExecutorResult<ExecutionOutput> {
        todo!()
    }

    async fn execute_finalizing_intent(
        &mut self,
        intent: ScheduledIntentBundle,
        commit_signature: Signature,
        pending_finalize_signature: Signature,
        execution_report: &mut IntentExecutionReport,
    ) -> IntentExecutorResult<ExecutionOutput> {

        todo!()
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
        let is_undelegate = intent.has_undelegate_intent();
        // TODO(edwin): should validate non emptiness of pubkeys? Shouldn't be possible tho
        let pubkeys = intent.get_all_committed_pubkeys();

        let mut execution_report = IntentExecutionReport::default();
        let result = match &self.stage {
            TwoStageProgress::Committing(signature) => {
                self.execute_committing_intent(intent, *signature, &mut execution_report).await
            }
            TwoStageProgress::Finalizing {
                commit,
                finalize
            } => {
                self.execute_finalizing_intent(intent, *commit, *finalize, &mut execution_report).await
            }
        };


        if is_undelegate {
            self.ctx
                .task_info_fetcher
                .reset(ResetType::Specific(&pubkeys));
        }

        todo!()
    }
}
