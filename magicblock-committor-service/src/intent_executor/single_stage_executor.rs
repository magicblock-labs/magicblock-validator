use std::ops::{ControlFlow, Deref};

use log::{error, info};
use magicblock_program::magic_scheduled_base_intent::ScheduledBaseIntent;

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
    // TODO: add strategy here?
}

impl<'a, T, F> SingleStageExecutor<'a, T, F>
where
    T: TransactionPreparator,
    F: TaskInfoFetcher,
{
    pub fn new(executor: &'a IntentExecutorImpl<T, F>) -> Self {
        Self { inner: executor }
    }

    pub async fn execute<P: IntentPersister>(
        &self,
        base_intent: ScheduledBaseIntent,
        mut transaction_strategy: TransactionStrategy,
        junk: &mut Vec<TransactionStrategy>,
        persister: &Option<P>,
    ) -> IntentExecutorResult<ExecutionOutput> {
        const RECURSION_CEILING: u8 = 10;

        let mut i = 0;
        let (execution_err, last_transaction_strategy) = loop {
            i += 1;

            // Prepare & execute message
            let execution_result = self
                .prepare_and_execute_strategy(
                    &mut transaction_strategy,
                    persister,
                )
                .await
                .map_err(IntentExecutorError::FailedFinalizePreparationError)?;
            // Process error: Ok - return, Err - handle further
            let execution_err = match execution_result {
                // break with result, strategy that was executed at this point has to be returned for cleanup
                Ok(value) => {
                    junk.push(transaction_strategy);
                    return Ok(ExecutionOutput::SingleStage(value));
                }
                Err(err) => err,
            };

            // Attempt patching
            let flow = self
                .patch_strategy(
                    &execution_err,
                    &mut transaction_strategy,
                    &base_intent,
                )
                .await?;
            let cleanup = match flow {
                ControlFlow::Continue(cleanup) => {
                    info!(
                        "Patched intent: {}. patched error: {:?}",
                        base_intent.id, execution_err
                    );
                    cleanup
                }
                ControlFlow::Break(()) => {
                    error!("Could not patch failed intent: {}", base_intent.id);
                    break (execution_err, transaction_strategy);
                }
            };

            if i >= RECURSION_CEILING {
                error!(
                    "CRITICAL! Recursion ceiling reached in intent execution."
                );
                break (execution_err, cleanup);
            } else {
                junk.push(cleanup);
            }
        };

        // Special case
        // TODO(edwin): maybe return and handle separately?
        let committed_pubkeys = base_intent.get_committed_pubkeys();
        if i < RECURSION_CEILING
            && matches!(
                execution_err,
                TransactionStrategyExecutionError::CpiLimitError
            )
            && committed_pubkeys.is_some()
        {
            // With actions, we can't predict num of CPIs
            // If we get here we will try to switch from Single stage to Two Stage commit
            // Note that this not necessarily will pass at the end due to the same reason

            // SAFETY: is_some() checked prior
            let committed_pubkeys = committed_pubkeys.unwrap();
            let (commit_strategy, finalize_strategy, cleanup) =
                self.handle_cpi_limit_error(last_transaction_strategy);
            junk.push(cleanup);
            self.two_stage_execution_flow(
                &committed_pubkeys,
                commit_strategy,
                finalize_strategy,
                persister,
            )
            .await
        } else {
            junk.push(last_transaction_strategy);
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
    }

    pub async fn patch_strategy(
        &self,
        err: &TransactionStrategyExecutionError,
        transaction_strategy: &mut TransactionStrategy,
        base_intent: &ScheduledBaseIntent,
    ) -> IntentExecutorResult<ControlFlow<(), TransactionStrategy>> {
        let Some(committed_pubkeys) = base_intent.get_committed_pubkeys()
        else {
            // No patching is applicable if intent doesn't commit accounts
            return Ok(ControlFlow::Break(()));
        };

        match err {
            TransactionStrategyExecutionError::ActionsError => {
                // Here we patch strategy for it to be retried in next iteration
                // & we also record data that has to be cleaned up after patch
                let to_cleanup =
                    self.handle_actions_error(transaction_strategy);
                Ok(ControlFlow::Continue(to_cleanup))
            }
            TransactionStrategyExecutionError::CommitIDError => {
                // Here we patch strategy for it to be retried in next iteration
                // & we also record data that has to be cleaned up after patch
                let to_cleanup = self
                    .handle_commit_id_error(
                        &committed_pubkeys,
                        transaction_strategy,
                    )
                    .await?;
                Ok(ControlFlow::Continue(to_cleanup))
            }
            TransactionStrategyExecutionError::CpiLimitError => {
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
