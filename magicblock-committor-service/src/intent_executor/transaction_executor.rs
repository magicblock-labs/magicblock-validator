use std::{mem, ops::ControlFlow};

use magicblock_core::traits::{ActionError, ActionsCallbackScheduler};
use solana_pubkey::Pubkey;
use solana_signature::Signature;
use solana_signer::Signer;
use tokio::time::timeout;
use tracing::{error, info};

use super::{
    error::{
        IntentExecutorError, IntentExecutorResult,
        TransactionStrategyExecutionError,
    },
    task_info_fetcher::TaskInfoFetcher,
    utils::{
        handle_actions_result, handle_commit_id_error, handle_cpi_limit_error,
        handle_undelegation_error, prepare_and_execute_strategy,
    },
    ExecutionOutput, IntentExecutionReport, IntentExecutorImpl,
};
use crate::{
    persist::IntentPersister,
    tasks::{
        task_strategist::{StrategyExecutionMode, TransactionStrategy},
        BaseTaskImpl,
    },
    transaction_preparator::TransactionPreparator,
};

#[cfg(test)]
mod tests;

/// Runs the selected transaction and any remaining actions or undelegations.
/// A confirmed commit is never included in subsequent recovery attempts.
pub(super) struct TransactionExecutor<'a, T, F, A> {
    executor: &'a IntentExecutorImpl<T, F, A>,
    report: &'a mut IntentExecutionReport,
    intent_id: u64,
    committed_pubkeys: &'a [Pubkey],
    current: TransactionStrategy,
    pending: Option<TransactionStrategy>,
    commit_signature: Option<Signature>,
    current_attempt: u8,
}

impl<'a, T, F, A> TransactionExecutor<'a, T, F, A>
where
    T: TransactionPreparator,
    F: TaskInfoFetcher,
    A: ActionsCallbackScheduler,
{
    pub(super) fn new(
        executor: &'a IntentExecutorImpl<T, F, A>,
        report: &'a mut IntentExecutionReport,
        intent_id: u64,
        committed_pubkeys: &'a [Pubkey],
        strategy: StrategyExecutionMode,
    ) -> Self {
        let (current, pending) = match strategy {
            StrategyExecutionMode::SingleStage(strategy) => (strategy, None),
            StrategyExecutionMode::TwoStage {
                commit_stage,
                finalize_stage,
            } => (commit_stage, Some(finalize_stage)),
        };
        Self {
            executor,
            report,
            intent_id,
            committed_pubkeys,
            current,
            pending,
            commit_signature: None,
            current_attempt: 0,
        }
    }

    pub(super) async fn execute<P: IntentPersister>(
        mut self,
        persister: &Option<P>,
    ) -> IntentExecutorResult<ExecutionOutput> {
        loop {
            let result = self.execute_with_timeout(persister).await;
            let result = match result {
                Err(IntentExecutorError::FailedToFinalizeError {
                    err, ..
                }) if self.is_combined()
                    && !self.committed_pubkeys.is_empty()
                    && self.has_tasks_after_commit()
                    && err.is_recoverable_by_two_stage() =>
                {
                    let (current, pending, cleanup) = handle_cpi_limit_error(
                        &self.executor.authority.pubkey(),
                        mem::take(&mut self.current),
                    );
                    self.current = current;
                    self.pending = Some(pending);
                    self.current_attempt = 0;
                    self.report.dispose(cleanup);
                    self.report.add_patched_error(err);
                    continue;
                }
                result => result,
            };

            // Combined execution reports all terminal errors to callbacks.
            // Split execution historically reports transaction outcomes, but
            // leaves preparation and nonce-fetch failures to the caller.
            if self.is_combined()
                || matches!(
                    &result,
                    Ok(_)
                        | Err(IntentExecutorError::FailedToCommitError { .. })
                        | Err(
                            IntentExecutorError::FailedToFinalizeError { .. }
                        )
                )
            {
                self.execute_callbacks(
                    result.as_ref().ok().copied(),
                    result.as_ref().map(|_| ()).map_err(ActionError::from),
                );
            }
            self.report.dispose(mem::take(&mut self.current));

            match result {
                Ok(signature) => {
                    if let Some(pending) = self.pending.take() {
                        self.current = pending;
                        self.commit_signature = Some(signature);
                        self.current_attempt = 0;
                    } else {
                        return Ok(match self.commit_signature {
                            Some(commit_signature) => {
                                ExecutionOutput::TwoStage {
                                    commit_signature,
                                    finalize_signature: signature,
                                }
                            }
                            None => ExecutionOutput::SingleStage(signature),
                        });
                    }
                }
                Err(err) => {
                    if let Some(pending) = self.pending.take() {
                        self.report.dispose(pending);
                    }
                    return Err(err);
                }
            }
        }
    }

    async fn execute_with_timeout<P: IntentPersister>(
        &mut self,
        persister: &Option<P>,
    ) -> IntentExecutorResult<Signature> {
        let has_callbacks = self.current.has_actions_callbacks()
            || self
                .pending
                .as_ref()
                .is_some_and(TransactionStrategy::has_actions_callbacks);
        if has_callbacks {
            if let Some(time_left) = self.executor.time_left() {
                if let Ok(result) =
                    timeout(time_left, self.execute_current(persister)).await
                {
                    return result;
                }
            }
            // A transaction may have landed before confirmation timed out.
            // The callback recipient handles that race via TimeoutError.
            info!("Intent execution timed out, cleaning up actions");
            self.execute_callbacks(None, Err(ActionError::TimeoutError));
        }
        self.execute_current(persister).await
    }

    #[tracing::instrument(skip_all, fields(stage =
        if self.pending.is_some() { "commit" }
        else if self.commit_signature.is_some() { "finalize" }
        else { "single_stage" }
    ))]
    async fn execute_current<P: IntentPersister>(
        &mut self,
        persister: &Option<P>,
    ) -> IntentExecutorResult<Signature> {
        const ATTEMPT_LIMIT: u8 = 10;
        let result = loop {
            self.current_attempt += 1;
            let result = prepare_and_execute_strategy(
                &self.executor.intent_client,
                &self.executor.authority,
                &self.executor.transaction_preparator,
                &mut self.current,
                persister,
            )
            .await
            .map_err(|err| {
                if self.pending.is_some() {
                    IntentExecutorError::FailedCommitPreparationError(err)
                } else {
                    IntentExecutorError::FailedFinalizePreparationError(err)
                }
            })?;
            let err = match result {
                Ok(signature) => break Ok(signature),
                Err(err) => err,
            };

            match self.patch_strategy(&err).await? {
                ControlFlow::Continue(cleanup) => self.report.dispose(cleanup),
                ControlFlow::Break(()) => break Err(err),
            }
            self.executor
                .intent_client
                .invalidate_cached_blockhash()
                .await;

            // Failed follow-up actions can leave no work after a landed commit.
            // An undelegation failure must still be returned if removing its
            // tasks empties the transaction.
            if let Some(signature) = self.commit_signature {
                if self.current.optimized_tasks.is_empty() {
                    if matches!(
                        err,
                        TransactionStrategyExecutionError::ActionsError(..)
                    ) {
                        self.report.add_patched_error(err);
                        break Ok(signature);
                    }
                    break Err(err);
                }
            }
            if self.current_attempt >= ATTEMPT_LIMIT {
                error!(attempt = self.current_attempt, error = ?err, "Transaction recovery attempt limit reached");
                break Err(err);
            }
            self.report.add_patched_error(err);
        };
        result.map_err(|err| {
            if self.pending.is_some() {
                IntentExecutorError::from_commit_execution_error(err)
            } else {
                IntentExecutorError::from_finalize_execution_error(
                    err,
                    self.commit_signature,
                )
            }
        })
    }

    async fn patch_strategy(
        &mut self,
        err: &TransactionStrategyExecutionError,
    ) -> IntentExecutorResult<ControlFlow<(), TransactionStrategy>> {
        if self.is_combined() && self.committed_pubkeys.is_empty() {
            return Ok(ControlFlow::Break(()));
        }
        let authority = self.executor.authority.pubkey();
        let cleanup = match err {
            TransactionStrategyExecutionError::CommitIDError(..)
                if self.commit_signature.is_none() =>
            {
                let cleanup = handle_commit_id_error(
                    &authority,
                    &self.executor.task_info_fetcher,
                    self.committed_pubkeys,
                    &mut self.current,
                    self.intent_id,
                )
                .await?;
                if let Some(pending) = &mut self.pending {
                    // Re-delegation can reset the nonce to 1. Both transactions
                    // then need the intent's uniqueness instruction.
                    if pending.uniqueness_nonce.is_none() {
                        pending.uniqueness_nonce =
                            self.current.uniqueness_nonce;
                    }
                }
                cleanup
            }
            TransactionStrategyExecutionError::ActionsError(err, signature) => {
                handle_actions_result(
                    &authority,
                    &self.executor.actions_callback_executor,
                    self.report,
                    &mut self.current,
                    *signature,
                    Err(ActionError::ActionsError(err.clone(), *signature)),
                )
            }
            TransactionStrategyExecutionError::UndelegationError(..)
                if self.pending.is_none() =>
            {
                handle_undelegation_error(&authority, &mut self.current)
            }
            _ => return Ok(ControlFlow::Break(())),
        };
        Ok(ControlFlow::Continue(cleanup))
    }

    fn execute_callbacks(
        &mut self,
        signature: Option<Signature>,
        result: Result<(), ActionError>,
    ) {
        let cleanup = handle_actions_result(
            &self.executor.authority.pubkey(),
            &self.executor.actions_callback_executor,
            self.report,
            &mut self.current,
            signature,
            result.clone(),
        );
        self.report.dispose(cleanup);
        if let (Err(_), Some(pending)) = (&result, &mut self.pending) {
            let cleanup = handle_actions_result(
                &self.executor.authority.pubkey(),
                &self.executor.actions_callback_executor,
                self.report,
                pending,
                signature,
                result,
            );
            self.report.dispose(cleanup);
        }
    }

    fn is_combined(&self) -> bool {
        self.pending.is_none() && self.commit_signature.is_none()
    }

    fn has_tasks_after_commit(&self) -> bool {
        self.current
            .optimized_tasks
            .iter()
            .rposition(|task| matches!(task, BaseTaskImpl::CommitFinalize(_)))
            .is_some_and(|index| index + 1 < self.current.optimized_tasks.len())
    }
}
