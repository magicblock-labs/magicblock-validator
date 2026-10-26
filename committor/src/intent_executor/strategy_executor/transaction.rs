use std::{mem, time::Duration};

use mbv_core::traits::{ActionError, ActionsCallbackScheduler};
use program::outbox::PendingTransaction;
use solana_pubkey::Pubkey;
use solana_rpc_client::rpc_client::SerializableTransaction;
use solana_signature::Signature;
use solana_signer::Signer;
use tokio::time::timeout;
use tracing::{error, info, warn};

use super::{
    error::TransactionExecutionError,
    recovery::TransactionRecovery,
    utils::{
        handle_actions_result, prepare_transaction, recovery_commit_nonces,
        resolve_pending_signature, split_for_follow_up,
    },
};
use crate::{
    intent_executor::{
        ExecutionOutput, IntentExecutionReport, IntentExecutorCtx,
        error::{IntentExecutorError, IntentExecutorResult},
        progress::{ExecutionProgress, TransactionProgress},
    },
    outbox::{
        OutboxClient, outbox_intent_bundles_reader::OutboxIntentBundlesReader,
        progress::PendingExecution,
    },
    tasks::{
        Task,
        task_info_fetcher::TaskInfoFetcher,
        task_strategist::{TransactionPlan, TransactionStrategy},
    },
    transaction_preparator::{TransactionPreparator, cleanup_resources::CleanupResources},
};

#[cfg(test)]
mod tests;

const TRANSACTION_LOOP_CEILING: u8 = 10;

/// Runs the selected transaction and any remaining actions or undelegations.
/// A confirmed commit is never included in subsequent recovery attempts.
pub(in crate::intent_executor) struct TransactionExecutor<'a, T, F, A, O> {
    ctx: &'a IntentExecutorCtx<T, F, A, O>,
    report: &'a mut IntentExecutionReport,
    intent_id: u64,
    committed_pubkeys: &'a [Pubkey],
    current: TransactionStrategy,
    follow_up: Option<TransactionStrategy>,
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
        plan: TransactionPlan,
    ) -> Self {
        let TransactionPlan { current, follow_up } = plan;
        Self {
            ctx,
            report,
            intent_id,
            committed_pubkeys,
            current,
            follow_up,
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
        let executor = Self::new(ctx, report, intent_id, &[], strategy.into());
        executor.report.progress.confirmed_commit = Some(commit_signature);
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
                        && err.is_recoverable_by_splitting() =>
                {
                    let (current, follow_up) = split_for_follow_up(
                        &self.ctx.authority.pubkey(),
                        mem::take(&mut self.current),
                    );
                    self.current = current;
                    self.follow_up = Some(follow_up);
                    self.current_attempt = 0;
                    self.report.add_patched_error(err);
                    continue;
                }
                result => result,
            };

            // Combined execution reports all terminal errors to callbacks.
            // Split execution leaves preparation/outbox/patch failures to the caller.
            let report_callbacks = self.is_combined() || result.is_ok();
            let result =
                result.and_then(|result| result.map_err(IntentExecutorError::FailedExecution));

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

            match result {
                Ok(signature) => {
                    if let Some(follow_up) = self.follow_up.take() {
                        self.current = follow_up;
                        self.report.progress = ExecutionProgress {
                            confirmed_commit: Some(signature),
                            ..Default::default()
                        };
                        self.pending_transaction = None;
                        self.current_attempt = 0;
                    } else {
                        let output = ExecutionOutput {
                            confirmed_commit: self.report.progress.confirmed_commit,
                            signature,
                        };
                        self.report.progress = output.progress();
                        return Ok(output);
                    }
                }
                Err(err) => return Err(err),
            }
        }
    }

    async fn execute_with_timeout(
        &mut self,
        time_left: Option<Duration>,
    ) -> IntentExecutorResult<Result<Signature, TransactionExecutionError>> {
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
        else if self.report.progress.confirmed_commit.is_some() { "follow_up" }
        else { "combined" }
    ))]
    async fn execute_current(
        &mut self,
    ) -> IntentExecutorResult<Result<Signature, TransactionExecutionError>> {
        let ctx = self.ctx;
        let confirmed_commit = self.report.progress.confirmed_commit;
        let has_follow_up = self.follow_up.is_some();
        let recovery = TransactionRecovery {
            authority: &ctx.authority,
            intent_client: &ctx.intent_client,
            intent_id: self.intent_id,
            callback_scheduler: &ctx.actions_callback_executor,
            task_info_fetcher: &ctx.task_info_fetcher,
            committed_pubkeys: self.committed_pubkeys,
        };
        let mut reconciled_outbox = false;
        loop {
            if self.pending_transaction.is_none() && !reconciled_outbox {
                reconciled_outbox = true;
                self.pending_transaction = self.recover_pending_from_outbox().await;
            }

            if let Some(ref pending) = self.pending_transaction {
                self.report.progress.transaction =
                    TransactionProgress::Submitted(pending.signature);
                if resolve_pending_signature(&ctx.intent_client, pending).await? {
                    self.report.progress.transaction =
                        TransactionProgress::Confirmed(pending.signature);
                    break Ok(Ok(pending.signature));
                }
                self.report.progress.transaction = TransactionProgress::Rejected(pending.signature);
                self.pending_transaction = None;
            }

            // Resolve a previously sent transaction before skipping an empty follow-up.
            if let Some(commit_signature) = confirmed_commit
                && self.current.tasks.is_empty()
            {
                return Ok(Ok(commit_signature));
            }

            self.current_attempt += 1;

            // Keep this attempt's resources outside the cancellable preparation future.
            let cleanup = self.report.cleanup.push_mut(CleanupResources::default());
            let prepared_transaction = prepare_transaction(
                &ctx.intent_client,
                &ctx.authority,
                &ctx.transaction_preparator,
                &self.current,
                cleanup,
            )
            .await
            .map_err(IntentExecutorError::FailedPreparation)?;

            // Record in outbox before sending
            let pending = PendingTransaction {
                signature: *prepared_transaction.get_signature(),
                blockhash: *prepared_transaction.get_recent_blockhash(),
            };
            self.report.progress.transaction = TransactionProgress::Prepared(pending.signature);
            let recovery_commit_nonces = recovery_commit_nonces(&self.current.tasks);
            ctx.outbox_client
                .set_intent_execution_stage(
                    self.intent_id,
                    PendingExecution {
                        transaction: pending,
                        confirmed_commit,
                        has_follow_up,
                    }
                    .into_stage(),
                    recovery_commit_nonces,
                )
                .await
                .map_err(Into::into)?;

            // Now record locally signature of tx we about to send
            // Precaution for timeout in between
            self.pending_transaction = Some(pending);
            self.report.progress.transaction = TransactionProgress::Submitted(pending.signature);
            let signature = &pending.signature;

            // Send signed tx
            let execution_result = ctx
                .intent_client
                .send_signed_tx_with_retries(&prepared_transaction, &self.current.tasks)
                .await;

            // Result returned, cleanup pending signature
            self.pending_transaction = None;

            let execution_err = match execution_result {
                Ok(()) => {
                    self.report.progress.transaction = TransactionProgress::Confirmed(*signature);
                    return Ok(Ok(*signature));
                }
                Err(err) => err,
            };
            if execution_err.is_rejected() {
                self.report.progress.transaction = TransactionProgress::Rejected(*signature);
            }
            let flow = recovery.recover(&execution_err, &mut self.current, self.report).await?;
            if flow.is_break() {
                return Ok(Err(execution_err));
            }
            ctx.intent_client.invalidate_cached_blockhash().await;

            if let Some(commit_signature) = confirmed_commit
                && self.current.tasks.is_empty()
            {
                // Failed actions can leave no work after a confirmed commit.
                // Keep undelegation failures visible even when patching removes all tasks.
                if matches!(execution_err, TransactionExecutionError::ActionsError(..)) {
                    self.report.add_patched_error(execution_err);
                    return Ok(Ok(commit_signature));
                }
                return Ok(Err(execution_err));
            }

            if self.current_attempt >= TRANSACTION_LOOP_CEILING {
                error!("CRITICAL! Recursion ceiling reached");
                return Ok(Err(execution_err));
            } else {
                self.report.add_patched_error(execution_err);
            }
        }
    }

    async fn recover_pending_from_outbox(&self) -> Option<PendingTransaction> {
        let reader = self.ctx.outbox_client.outbox_reader();
        match reader.fetch_outbox_intent(self.intent_id).await {
            Ok(Some(bundle)) => PendingExecution::from_status(bundle.status())
                .filter(|pending| {
                    pending.has_follow_up == self.follow_up.is_some()
                        && pending.confirmed_commit.is_some()
                            == self.report.progress.confirmed_commit.is_some()
                })
                .map(|pending| pending.transaction),
            Ok(None) => None,
            Err(_) => {
                warn!(
                    intent_id = self.intent_id,
                    "Failed to reconcile pending transaction from outbox"
                );
                None
            }
        }
    }

    fn execute_callbacks(&mut self, signature: Option<Signature>, result: Result<(), ActionError>) {
        handle_actions_result(
            &self.ctx.authority.pubkey(),
            &self.ctx.actions_callback_executor,
            self.report,
            &mut self.current,
            signature,
            result.clone(),
        );
        if let (Err(_), Some(follow_up)) = (&result, &mut self.follow_up) {
            handle_actions_result(
                &self.ctx.authority.pubkey(),
                &self.ctx.actions_callback_executor,
                self.report,
                follow_up,
                signature,
                result,
            );
        }
    }

    fn is_combined(&self) -> bool {
        self.follow_up.is_none() && self.report.progress.confirmed_commit.is_none()
    }

    fn has_tasks_after_commit(&self) -> bool {
        self.current
            .tasks
            .iter()
            .rposition(|task| matches!(task, Task::CommitFinalize(_)))
            .is_some_and(|index| index + 1 < self.current.tasks.len())
    }
}
