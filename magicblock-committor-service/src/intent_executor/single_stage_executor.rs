use std::ops::{ControlFlow, Deref};

use log::error;
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

pub struct SingleStageExecutor<'a, T, F> {
    inner: &'a IntentExecutorImpl<T, F>,
    pub transaction_strategy: TransactionStrategy,

    /// Junk that needs to be cleaned up
    pub junk: Vec<TransactionStrategy>,
    /// Errors we patched trying to recover intent
    pub patched_errors: Vec<TransactionStrategyExecutionError>,
}

impl<'a, T, F> SingleStageExecutor<'a, T, F>
where
    T: TransactionPreparator + Clone,
    F: TaskInfoFetcher,
{
    pub fn new(
        executor: &'a IntentExecutorImpl<T, F>,
        transaction_strategy: TransactionStrategy,
    ) -> Self {
        Self {
            inner: executor,
            transaction_strategy,
            junk: vec![],
            patched_errors: vec![],
        }
    }

    pub async fn execute<P: IntentPersister>(
        &mut self,
        committed_pubkeys: Option<&[Pubkey]>,
        persister: &Option<P>,
    ) -> IntentExecutorResult<ExecutionOutput> {
        const RECURSION_CEILING: u8 = 10;

        let mut i = 0;
        let execution_err = loop {
            i += 1;

            // Prepare & execute message
            let execution_result = self
                .inner
                .prepare_and_execute_strategy(
                    &mut self.transaction_strategy,
                    persister,
                )
                .await
                .map_err(IntentExecutorError::FailedFinalizePreparationError)?;
            // Process error: Ok - return, Err - handle further
            let execution_err = match execution_result {
                // break with result, strategy that was executed at this point has to be returned for cleanup
                Ok(value) => {
                    return Ok(ExecutionOutput::SingleStage(value));
                }
                Err(err) => err,
            };

            // Attempt patching
            let flow = Self::patch_strategy(
                self.inner,
                &execution_err,
                &mut self.transaction_strategy,
                committed_pubkeys,
            )
            .await?;
            let cleanup = match flow {
                ControlFlow::Continue(cleanup) => cleanup,
                ControlFlow::Break(()) => {
                    break execution_err;
                }
            };
            self.junk.push(cleanup);

            if i >= RECURSION_CEILING {
                error!(
                    "CRITICAL! Recursion ceiling reached in intent execution."
                );
                break execution_err;
            } else {
                self.patched_errors.push(execution_err);
            }
        };

        // Special case
        let err = IntentExecutorError::from_strategy_execution_error(
            execution_err,
            |internal_err| {
                let signature = internal_err.signature();
                IntentExecutorError::FailedToFinalizeError {
                    err: internal_err,
                    commit_signature: signature,
                    finalize_signature: signature,
                }
            },
        );

        Err(err)
    }

    /// Patch the current `transaction_strategy` in response to a recoverable
    /// [`TransactionStrategyExecutionError`], optionally preparing cleanup data
    /// to be applied after a retry.
    ///
    /// [`TransactionStrategyExecutionError`], returning either:
    /// - `Continue(to_cleanup)` when a retry should be attempted with cleanup metadata, or
    /// - `Break(())` when this stage cannot be recovered here.
    pub async fn patch_strategy(
        inner: &IntentExecutorImpl<T, F>,
        err: &TransactionStrategyExecutionError,
        transaction_strategy: &mut TransactionStrategy,
        committed_pubkeys: Option<&[Pubkey]>,
    ) -> IntentExecutorResult<ControlFlow<(), TransactionStrategy>> {
        let Some(committed_pubkeys) = committed_pubkeys else {
            // No patching is applicable if intent doesn't commit accounts
            return Ok(ControlFlow::Break(()));
        };

        match err {
            TransactionStrategyExecutionError::ActionsError(_, _) => {
                // Here we patch strategy for it to be retried in next iteration
                // & we also record data that has to be cleaned up after patch
                let to_cleanup =
                    inner.handle_actions_error(transaction_strategy);
                Ok(ControlFlow::Continue(to_cleanup))
            }
            TransactionStrategyExecutionError::CommitIDError(_, _) => {
                // Here we patch strategy for it to be retried in next iteration
                // & we also record data that has to be cleaned up after patch
                let to_cleanup = inner
                    .handle_commit_id_error(
                        committed_pubkeys,
                        transaction_strategy,
                    )
                    .await?;
                Ok(ControlFlow::Continue(to_cleanup))
            }
            TransactionStrategyExecutionError::UndelegationError(_, _) => {
                // Here we patch strategy for it to be retried in next iteration
                // & we also record data that has to be cleaned up after patch
                let to_cleanup =
                    inner.handle_undelegation_error(transaction_strategy);
                Ok(ControlFlow::Continue(to_cleanup))
            }
            TransactionStrategyExecutionError::CpiLimitError(_, _) => {
                // Can't be handled in scope of single stage execution
                // We signal flow break
                Ok(ControlFlow::Break(()))
            }
            TransactionStrategyExecutionError::InternalError(_) => {
                // Error that we can't handle - break with cleanup data
                Ok(ControlFlow::Break(()))
            }
        }
    }
}

impl<'a, T, F> Deref for SingleStageExecutor<'a, T, F> {
    type Target = IntentExecutorImpl<T, F>;

    fn deref(&self) -> &'a Self::Target {
        self.inner
    }
}
