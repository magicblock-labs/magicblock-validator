use std::{mem, time::Duration};

use mbv_core::traits::{ActionError, ActionsCallbackScheduler};
use program::outbox::{ExecutionStage, PendingTransaction, TwoStageProgress};
use solana_pubkey::Pubkey;
use solana_signature::Signature;
use solana_signer::Signer;
use tokio::time::timeout;
use tracing::info;

use super::{
    error::TransactionStrategyExecutionError,
    patcher::{CommitStagePatcher, FinalizeStagePatcher, SingleStagePatcher},
    utils::{
        ExecutionState, committing_pending, finalizing_pending, handle_actions_result,
        handle_cpi_limit_error, single_stage_pending, stage_execution_loop,
    },
};
use crate::{
    intent_executor::{
        ExecutionOutput, IntentExecutionReport, IntentExecutorCtx,
        error::{IntentExecutorError, IntentExecutorResult},
    },
    outbox::OutboxClient,
    tasks::{
        BaseTaskImpl,
        task_info_fetcher::TaskInfoFetcher,
        task_strategist::{StrategyExecutionMode, TransactionStrategy, TwoStageExecutionMode},
    },
    transaction_preparator::TransactionPreparator,
};

#[cfg(test)]
mod tests;

/// Runs the selected transaction and any remaining actions or undelegations.
/// A confirmed commit is never included in subsequent recovery attempts.
pub(in crate::intent_executor) struct TransactionExecutor<'a, T, F, A, O> {
    ctx: &'a IntentExecutorCtx<T, F, A, O>,
    report: &'a mut IntentExecutionReport,
    intent_id: u64,
    committed_pubkeys: &'a [Pubkey],
    current: TransactionStrategy,
    follow_up: Option<TransactionStrategy>,
    commit_signature: Option<Signature>,
    pending_transaction: Option<PendingTransaction>,
    current_attempt: u8,
}

impl<'a, T, F, A, O> TransactionExecutor<'a, T, F, A, O>
where
    T: TransactionPreparator,
    F: TaskInfoFetcher,
    A: ActionsCallbackScheduler,
    O: OutboxClient,
    O::Error: Into<IntentExecutorError>,
{
    pub(in crate::intent_executor) fn new(
        ctx: &'a IntentExecutorCtx<T, F, A, O>,
        report: &'a mut IntentExecutionReport,
        intent_id: u64,
        committed_pubkeys: &'a [Pubkey],
        strategy: StrategyExecutionMode,
    ) -> Self {
        let (current, follow_up) = match strategy {
            StrategyExecutionMode::SingleStage(strategy) => (strategy, None),
            StrategyExecutionMode::TwoStage(TwoStageExecutionMode {
                commit_stage,
                finalize_stage,
            }) => (commit_stage, Some(finalize_stage)),
        };
        Self {
            ctx,
            report,
            intent_id,
            committed_pubkeys,
            current,
            follow_up,
            commit_signature: None,
            pending_transaction: None,
            current_attempt: 0,
        }
    }

    pub(in crate::intent_executor) fn resume_follow_up(
        ctx: &'a IntentExecutorCtx<T, F, A, O>,
        report: &'a mut IntentExecutionReport,
        intent_id: u64,
        commit_signature: Signature,
        strategy: TransactionStrategy,
    ) -> Self {
        let mut executor = Self::new(
            ctx,
            report,
            intent_id,
            &[],
            StrategyExecutionMode::SingleStage(strategy),
        );
        executor.commit_signature = Some(commit_signature);
        executor
    }

    pub(in crate::intent_executor) async fn execute(
        mut self,
        time_left: impl Fn() -> Option<Duration>,
    ) -> IntentExecutorResult<ExecutionOutput> {
        loop {
            let result = self.execute_with_timeout(time_left()).await;
            let result = match result {
                Ok(Err(err))
                    if self.is_combined()
                        && !self.committed_pubkeys.is_empty()
                        && self.has_tasks_after_commit()
                        && err.is_recoverable_by_two_stage() =>
                {
                    let (current, follow_up, cleanup) = handle_cpi_limit_error(
                        &self.ctx.authority.pubkey(),
                        mem::take(&mut self.current),
                    );
                    self.current = current;
                    self.follow_up = Some(follow_up);
                    self.current_attempt = 0;
                    self.report.dispose(cleanup);
                    self.report.add_patched_error(err);
                    continue;
                }
                result => result,
            };

            // Combined execution reports all terminal errors to callbacks.
            // Split execution leaves preparation/outbox/patch failures to the caller.
            let report_callbacks = self.is_combined() || result.is_ok();
            let result = match result {
                Ok(result) => result.map_err(|err| {
                    if self.follow_up.is_some() {
                        IntentExecutorError::from_commit_execution_error(err)
                    } else {
                        IntentExecutorError::from_finalize_execution_error(
                            err,
                            self.commit_signature,
                        )
                    }
                }),
                Err(err) => Err(err),
            };

            if let Some(follow_up) = &mut self.follow_up {
                // A nonce retry can turn this into a first commit; both transactions
                // then need the same per-intent uniqueness instruction.
                follow_up.uniqueness_nonce = self.current.uniqueness_nonce;
            }
            if report_callbacks {
                self.execute_callbacks(
                    result.as_ref().ok().copied(),
                    result.as_ref().map(|_| ()).map_err(ActionError::from),
                );
            }
            self.report.dispose(mem::take(&mut self.current));

            match result {
                Ok(signature) => {
                    if let Some(follow_up) = self.follow_up.take() {
                        self.current = follow_up;
                        self.commit_signature = Some(signature);
                        self.pending_transaction = None;
                        self.current_attempt = 0;
                    } else {
                        return Ok(match self.commit_signature {
                            Some(commit_signature) => ExecutionOutput::TwoStage {
                                commit_signature,
                                finalize_signature: signature,
                            },
                            None => ExecutionOutput::SingleStage(signature),
                        });
                    }
                }
                Err(err) => {
                    if let Some(follow_up) = self.follow_up.take() {
                        self.report.dispose(follow_up);
                    }
                    return Err(err);
                }
            }
        }
    }

    async fn execute_with_timeout(
        &mut self,
        time_left: Option<Duration>,
    ) -> IntentExecutorResult<Result<Signature, TransactionStrategyExecutionError>> {
        if self.current.has_actions_callbacks()
            || self.follow_up.as_ref().is_some_and(TransactionStrategy::has_actions_callbacks)
        {
            if let Some(time_left) = time_left
                && let Ok(result) = timeout(time_left, self.execute_current()).await
            {
                return result;
            }
            // Only callbacks expire. Keep actions and resolve any pending transaction
            // before preparing another one, since the cancelled send may still land.
            info!("Intent callbacks timed out; continuing execution");
            self.execute_callbacks(None, Err(ActionError::TimeoutError));
        }
        self.execute_current().await
    }

    #[tracing::instrument(skip_all, fields(stage =
        if self.follow_up.is_some() { "commit" }
        else if self.commit_signature.is_some() { "finalize" }
        else { "single_stage" }
    ))]
    async fn execute_current(
        &mut self,
    ) -> IntentExecutorResult<Result<Signature, TransactionStrategyExecutionError>> {
        let ctx = self.ctx;
        let state = ExecutionState {
            current_attempt: &mut self.current_attempt,
            transaction_strategy: &mut self.current,
            pending_transaction: &mut self.pending_transaction,
            execution_report: self.report,
            commit_signature: self.commit_signature,
        };
        if self.follow_up.is_some() {
            stage_execution_loop(
                &ctx.authority,
                &ctx.intent_client,
                ctx.outbox_client.as_ref(),
                &ctx.transaction_preparator,
                CommitStagePatcher {
                    authority: &ctx.authority,
                    intent_client: &ctx.intent_client,
                    intent_id: self.intent_id,
                    callback_scheduler: &ctx.actions_callback_executor,
                    task_info_fetcher: &ctx.task_info_fetcher,
                    committed_pubkeys: self.committed_pubkeys,
                },
                self.intent_id,
                |pending| ExecutionStage::TwoStage(TwoStageProgress::Committing(pending)),
                committing_pending,
                IntentExecutorError::FailedCommitPreparationError,
                state,
            )
            .await
        } else if let Some(commit) = self.commit_signature {
            stage_execution_loop(
                &ctx.authority,
                &ctx.intent_client,
                ctx.outbox_client.as_ref(),
                &ctx.transaction_preparator,
                FinalizeStagePatcher {
                    authority: &ctx.authority,
                    callback_scheduler: &ctx.actions_callback_executor,
                },
                self.intent_id,
                |finalize| {
                    ExecutionStage::TwoStage(TwoStageProgress::Finalizing { commit, finalize })
                },
                finalizing_pending,
                IntentExecutorError::FailedFinalizePreparationError,
                state,
            )
            .await
        } else {
            stage_execution_loop(
                &ctx.authority,
                &ctx.intent_client,
                ctx.outbox_client.as_ref(),
                &ctx.transaction_preparator,
                SingleStagePatcher {
                    authority: &ctx.authority,
                    intent_client: &ctx.intent_client,
                    intent_id: self.intent_id,
                    callback_scheduler: &ctx.actions_callback_executor,
                    task_info_fetcher: &ctx.task_info_fetcher,
                    committed_pubkeys: self.committed_pubkeys,
                },
                self.intent_id,
                ExecutionStage::SingleStage,
                single_stage_pending,
                IntentExecutorError::FailedFinalizePreparationError,
                state,
            )
            .await
        }
    }

    fn execute_callbacks(&mut self, signature: Option<Signature>, result: Result<(), ActionError>) {
        let cleanup = handle_actions_result(
            &self.ctx.authority.pubkey(),
            &self.ctx.actions_callback_executor,
            self.report,
            &mut self.current,
            signature,
            result.clone(),
        );
        self.report.dispose(cleanup);
        if let (Err(_), Some(follow_up)) = (&result, &mut self.follow_up) {
            let cleanup = handle_actions_result(
                &self.ctx.authority.pubkey(),
                &self.ctx.actions_callback_executor,
                self.report,
                follow_up,
                signature,
                result,
            );
            self.report.dispose(cleanup);
        }
    }

    fn is_combined(&self) -> bool {
        self.follow_up.is_none() && self.commit_signature.is_none()
    }

    fn has_tasks_after_commit(&self) -> bool {
        self.current
            .optimized_tasks
            .iter()
            .rposition(|task| matches!(task, BaseTaskImpl::CommitFinalize(_)))
            .is_some_and(|index| index + 1 < self.current.optimized_tasks.len())
    }
}
