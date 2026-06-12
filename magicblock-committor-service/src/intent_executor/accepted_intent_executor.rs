use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use async_trait::async_trait;
use futures_util::future::{join, try_join_all};
use magicblock_core::traits::ActionsCallbackScheduler;
use magicblock_program::{
    magic_scheduled_base_intent::ScheduledIntentBundle,
    outbox_intent_bundles::OutboxIntentBundle, validator::validator_authority,
};
use magicblock_rpc_client::MagicblockRpcClient;
use solana_keypair::{Address as Pubkey, Keypair};
use solana_signer::Signer;
use tracing::trace;

use crate::{
    intent_executor::{
        cleanup_handle::CleanupHandle,
        error::{IntentExecutorError, IntentExecutorResult},
        intent_execution_client::IntentExecutionClient,
        strategy_executor::{
            single_stage::SingleStageStrategyExecutor,
            two_stage::{Initialized, TwoStageStrategyExecutor},
            utils::{
                execute_with_timeout, handle_cpi_limit_error, CommitStage,
                FinalizeStage, SingleStage,
            },
        },
        task_info_fetcher::{CacheTaskInfoFetcher, ResetType, TaskInfoFetcher},
        ExecutionOutput, IntentExecutionReport, IntentExecutionResult,
        IntentExecutor, IntentExecutorCtx,
    },
    outbox_client::OutboxClient,
    tasks::{
        task_builder::TasksBuilder,
        task_strategist::{
            StrategyExecutionMode, TaskStrategist, TransactionStrategy,
        },
        TaskBuilderImpl,
    },
    transaction_preparator::{
        delivery_preparator::BufferExecutionError, TransactionPreparator,
    },
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
        let (commit_tasks, finalize_tasks) = {
            let commit_tasks_fut = TaskBuilderImpl::commit_tasks(
                &self.ctx.task_info_fetcher,
                &intent_bundle,
            );
            let finalize_tasks_fut = TaskBuilderImpl::finalize_tasks(
                &self.ctx.task_info_fetcher,
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
                    intent_bundle.id,
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
        self.ctx
            .actions_timeout
            .checked_sub(self.started_at.elapsed())
    }

    /// Starting execution from single stage
    pub async fn single_stage_execution_flow(
        &mut self,
        base_intent: ScheduledIntentBundle,
        transaction_strategy: TransactionStrategy,
        execution_report: &mut IntentExecutionReport,
    ) -> IntentExecutorResult<ExecutionOutput> {
        let committed_pubkeys = base_intent.get_all_committed_pubkeys();

        let mut single_stage_executor = SingleStageStrategyExecutor::new(
            self.authority.insecure_clone(),
            base_intent.id,
            None,
            self.ctx.intent_client.clone(),
            self.ctx.task_info_fetcher.clone(),
            self.ctx.outbox_client.clone(),
            transaction_strategy,
            self.ctx.actions_callback_executor.clone(),
            execution_report,
        );
        let res = execute_with_timeout(
            self.time_left(),
            SingleStage {
                inner: &mut single_stage_executor,
                transaction_preparator: &self.ctx.transaction_preparator,
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
            base_intent.id,
            &committed_pubkeys,
            commit_strategy,
            finalize_strategy,
            execution_report,
        )
        .await
    }

    pub async fn two_stage_execution_flow(
        &mut self,
        intent_id: u64,
        committed_pubkeys: &[Pubkey],
        commit_strategy: TransactionStrategy,
        finalize_strategy: TransactionStrategy,
        execution_report: &mut IntentExecutionReport,
    ) -> IntentExecutorResult<ExecutionOutput> {
        let state = Initialized::new(commit_strategy, finalize_strategy, None);
        let mut executor = TwoStageStrategyExecutor::new(
            state,
            self.authority.insecure_clone(),
            intent_id,
            self.ctx.intent_client.clone(),
            self.ctx.outbox_client.clone(),
            self.ctx.actions_callback_executor.clone(),
            execution_report,
        );

        let commit_signature = execute_with_timeout(
            self.time_left(),
            CommitStage {
                inner: &mut executor,
                transaction_preparator: &self.ctx.transaction_preparator,
                task_info_fetcher: &self.ctx.task_info_fetcher,
                committed_pubkeys,
            },
        )
        .await?;

        let mut finalize_executor = executor.done(commit_signature);
        let finalize_signature = execute_with_timeout(
            self.time_left(),
            FinalizeStage {
                inner: &mut finalize_executor,
                transaction_preparator: &self.ctx.transaction_preparator,
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
