use std::ops::{ControlFlow, Deref};

use log::{error, info, warn};
use solana_pubkey::Pubkey;

use crate::{
    intent_executor::{
        error::{
            IntentExecutorError, IntentExecutorResult,
            TransactionStrategyExecutionError,
        },
        task_info_fetcher::TaskInfoFetcher,
        ExecutionOutput, IntentExecutorImpl,
    },
    persist::IntentPersister,
    tasks::task_strategist::TransactionStrategy,
    transaction_preparator::TransactionPreparator,
};

pub struct TwoStageExecutor<'a, T, F> {
    inner: &'a IntentExecutorImpl<T, F>,
}

impl<'a, T, F> TwoStageExecutor<'a, T, F>
where
    T: TransactionPreparator + Clone,
    F: TaskInfoFetcher,
{
    pub fn new(executor: &'a IntentExecutorImpl<T, F>) -> Self {
        Self { inner: executor }
    }

    pub async fn execute<P: IntentPersister>(
        &self,
        committed_pubkeys: &[Pubkey],
        mut commit_strategy: TransactionStrategy,
        mut finalize_strategy: TransactionStrategy,
        junk: &mut Vec<TransactionStrategy>,
        persister: &Option<P>,
    ) -> IntentExecutorResult<ExecutionOutput> {
        const RECURSION_CEILING: u8 = 10;

        let mut i = 0;
        let (commit_result, last_commit_strategy) = loop {
            i += 1;

            // Prepare & execute message
            let execution_result = self
                .prepare_and_execute_strategy(&mut commit_strategy, persister)
                .await
                .map_err(IntentExecutorError::FailedCommitPreparationError)?;
            let execution_err = match execution_result {
                Ok(value) => break (Ok(value), commit_strategy),
                Err(err) => err,
            };

            let flow = self
                .patch_commit_strategy(
                    &execution_err,
                    &mut commit_strategy,
                    committed_pubkeys,
                )
                .await?;
            let cleanup = match flow {
                ControlFlow::Continue(value) => {
                    info!("Patched intent, error was: {:?}", execution_err);
                    value
                }
                ControlFlow::Break(()) => {
                    break (Err(execution_err), commit_strategy)
                }
            };

            if i >= RECURSION_CEILING {
                error!(
                    "CRITICAL! Recursion ceiling reached in intent execution."
                );
                break (Err(execution_err), cleanup);
            } else {
                junk.push(cleanup);
            }
        };

        junk.push(last_commit_strategy);
        let commit_signature = commit_result.map_err(|err| {
            IntentExecutorError::from_strategy_execution_error(
                err,
                |internal_err| {
                    let signature = internal_err.signature();
                    IntentExecutorError::FailedToCommitError {
                        err: internal_err,
                        signature,
                    }
                },
            )
        })?;

        i = 0;
        let (finalize_result, last_finalize_strategy) = loop {
            i += 1;

            // Prepare & execute message
            let execution_result = self
                .prepare_and_execute_strategy(&mut finalize_strategy, persister)
                .await
                .map_err(IntentExecutorError::FailedFinalizePreparationError)?;
            let execution_err = match execution_result {
                Ok(value) => break (Ok(value), finalize_strategy),
                Err(err) => err,
            };

            let flow = self
                .patch_finalize_strategy(&execution_err, &mut finalize_strategy)
                .await?;

            let cleanup = match flow {
                ControlFlow::Continue(cleanup) => cleanup,
                ControlFlow::Break(()) => {
                    break (Err(execution_err), finalize_strategy)
                }
            };

            if i >= RECURSION_CEILING {
                error!(
                    "CRITICAL! Recursion ceiling reached in intent execution."
                );
                break (Err(execution_err), cleanup);
            } else {
                junk.push(cleanup);
            }
        };

        junk.push(last_finalize_strategy);
        let finalize_signature = finalize_result.map_err(|err| {
            IntentExecutorError::from_strategy_execution_error(
                err,
                |internal_err| {
                    let finalize_signature = internal_err.signature();
                    IntentExecutorError::FailedToFinalizeError {
                        err: internal_err,
                        commit_signature: Some(commit_signature),
                        finalize_signature,
                    }
                },
            )
        })?;

        Ok(ExecutionOutput::TwoStage {
            commit_signature,
            finalize_signature,
        })
    }

    /// Patches Commit stage `transaction_strategy` in response to a recoverable
    /// [`TransactionStrategyExecutionError`], optionally preparing cleanup data
    /// to be applied after a retry.
    ///
    /// [`TransactionStrategyExecutionError`], returning either:
    /// - `Continue(to_cleanup)` when a retry should be attempted with cleanup metadata, or
    /// - `Break(())` when this stage cannot be recovered.
    pub async fn patch_commit_strategy(
        &self,
        err: &TransactionStrategyExecutionError,
        commit_strategy: &mut TransactionStrategy,
        committed_pubkeys: &[Pubkey],
    ) -> IntentExecutorResult<ControlFlow<(), TransactionStrategy>> {
        match err {
            TransactionStrategyExecutionError::CommitIDError(_, _) => {
                let to_cleanup = self
                    .handle_commit_id_error(committed_pubkeys, commit_strategy)
                    .await?;
                Ok(ControlFlow::Continue(to_cleanup))
            }
            TransactionStrategyExecutionError::ActionsError(_, _) => {
                // Unexpected in Two Stage commit
                // That would mean that Two Stage executes Standalone commit
                error!("Unexpected error in two stage commit flow: {}", err);
                Ok(ControlFlow::Break(()))
            }
            TransactionStrategyExecutionError::UndelegationError(_, _) => {
                // Unexpected in Two Stage commit
                // That would mean that Two Stage executes undelegation in commit phase
                error!("Unexpected error in two stage commit flow: {}", err);
                Ok(ControlFlow::Break(()))
            }
            TransactionStrategyExecutionError::CpiLimitError(_, _) => {
                // Can't be handled
                error!("Commit tasks exceeded CpiLimitError: {}", err);
                Ok(ControlFlow::Break(()))
            }
            TransactionStrategyExecutionError::InternalError(_) => {
                // Can't be handled
                Ok(ControlFlow::Break(()))
            }
        }
    }

    /// Patches Finalize stage `transaction_strategy` in response to a recoverable
    /// [`TransactionStrategyExecutionError`], optionally preparing cleanup data
    /// to be applied after a retry.
    ///
    /// [`TransactionStrategyExecutionError`], returning either:
    /// - `Continue(to_cleanup)` when a retry should be attempted with cleanup metadata, or
    /// - `Break(())` when this stage cannot be recovered.
    pub async fn patch_finalize_strategy(
        &self,
        err: &TransactionStrategyExecutionError,
        finalize_strategy: &mut TransactionStrategy,
    ) -> IntentExecutorResult<ControlFlow<(), TransactionStrategy>> {
        match err {
            TransactionStrategyExecutionError::CommitIDError(_, _) => {
                // Unexpected error in Two Stage commit
                error!("Unexpected error in Two stage commit flow: {}", err);
                Ok(ControlFlow::Break(()))
            }
            TransactionStrategyExecutionError::ActionsError(_, _) => {
                // Here we patch strategy for it to be retried in next iteration
                // & we also record data that has to be cleaned up after patch
                let to_cleanup = self.handle_actions_error(finalize_strategy);
                Ok(ControlFlow::Continue(to_cleanup))
            }
            TransactionStrategyExecutionError::UndelegationError(_, _) => {
                // Here we patch strategy for it to be retried in next iteration
                // & we also record data that has to be cleaned up after patch
                let to_cleanup =
                    self.handle_undelegation_error(finalize_strategy);
                Ok(ControlFlow::Continue(to_cleanup))
            }
            TransactionStrategyExecutionError::CpiLimitError(_, _) => {
                // Can't be handled
                warn!("Finalization tasks exceeded CpiLimitError: {}", err);
                Ok(ControlFlow::Break(()))
            }
            TransactionStrategyExecutionError::InternalError(_) => {
                // Can't be handled
                Ok(ControlFlow::Break(()))
            }
        }
    }
}

impl<'a, T, F> Deref for TwoStageExecutor<'a, T, F> {
    type Target = IntentExecutorImpl<T, F>;

    fn deref(&self) -> &'a Self::Target {
        self.inner
    }
}
