use std::ops::ControlFlow;

use solana_pubkey::Pubkey;
use solana_signature::Signature;
use tracing::{error, instrument};

use crate::{
    intent_executor::{
        error::{
            IntentExecutorError, IntentExecutorResult,
            TransactionStrategyExecutionError,
        },
        task_info_fetcher::TaskInfoFetcher,
        CommitSlotFn, ExecutionOutput, IntentExecutorImpl,
    },
    persist::{IntentPersister, IntentPersisterImpl},
    tasks::{
        args_task::{ArgsTask, ArgsTaskType},
        task_strategist::TransactionStrategy,
        task_visitors::utility_visitor::TaskVisitorUtils,
        BaseTask,
    },
    transaction_preparator::TransactionPreparator,
};

pub struct SingleStageExecutor<'a, T, F> {
    pub(in crate::intent_executor) inner: &'a mut IntentExecutorImpl<T, F>,
    pub transaction_strategy: TransactionStrategy,
}

impl<'a, T, F> SingleStageExecutor<'a, T, F>
where
    T: TransactionPreparator,
    F: TaskInfoFetcher,
{
    pub fn new(
        executor: &'a mut IntentExecutorImpl<T, F>,
        transaction_strategy: TransactionStrategy,
    ) -> Self {
        Self {
            inner: executor,
            transaction_strategy,
        }
    }

    #[instrument(
        skip(self, committed_pubkeys, persister),
        fields(stage = "single_stage")
    )]
    pub async fn execute<P: IntentPersister>(
        &mut self,
        committed_pubkeys: Option<&[Pubkey]>,
        persister: &Option<P>,
    ) -> IntentExecutorResult<ExecutionOutput> {
        const RECURSION_CEILING: u8 = 10;

        let mut i = 0;
        let result = loop {
            i += 1;

            // Prepare & execute message
            let execution_result = self
                .inner
                .prepare_and_execute_strategy(
                    &mut self.transaction_strategy,
                    persister,
                    None,
                )
                .await
                .map_err(IntentExecutorError::FailedFinalizePreparationError)?;

            // Process error: Ok - return, Err - handle further
            let execution_err = match execution_result {
                // break with result, strategy that was executed at this point has to be returned for cleanup
                Ok(value) => {
                    break Ok(ExecutionOutput::SingleStage(value));
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
                    break Err(execution_err);
                }
            };
            self.inner.junk.push(cleanup);

            if i >= RECURSION_CEILING {
                error!(
                    attempt = i,
                    ceiling = RECURSION_CEILING,
                    error = ?execution_err,
                    "Recursion ceiling exceeded"
                );
                break Err(execution_err);
            } else {
                self.inner.patched_errors.push(execution_err);
            }
        };

        result.map_err(|err| {
            IntentExecutorError::from_finalize_execution_error(
                err,
                // TODO(edwin): shall one stage have same signature for commit & finalize
                None,
            )
        })
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
            err
            @ TransactionStrategyExecutionError::UnfinalizedAccountError(
                _,
                signature,
            ) => {
                let optimized_tasks =
                    transaction_strategy.optimized_tasks.as_slice();
                if let Some(task) = err
                    .task_index()
                    .and_then(|index| optimized_tasks.get(index as usize))
                {
                    Self::handle_unfinalized_account_error(
                        inner,
                        signature,
                        task.as_ref(),
                    )
                    .await
                } else {
                    error!(
                        task_index = err.task_index(),
                        optimized_tasks_len = optimized_tasks.len(),
                        error = ?err,
                        "RPC returned unexpected task index"
                    );
                    Ok(ControlFlow::Break(()))
                }
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

    /// Handles unfinalized account error
    /// Sends a separate tx to finalize account and then continues execution
    async fn handle_unfinalized_account_error(
        inner: &IntentExecutorImpl<T, F>,
        failed_signature: &Option<Signature>,
        task: &dyn BaseTask,
    ) -> IntentExecutorResult<ControlFlow<(), TransactionStrategy>> {
        let Some(commit_meta) = TaskVisitorUtils::commit_meta(task) else {
            // Can't recover - break execution
            return Ok(ControlFlow::Break(()));
        };
        let finalize_task: Box<dyn BaseTask> =
            Box::new(ArgsTask::new(ArgsTaskType::Finalize(commit_meta.into())));
        inner
            .prepare_and_execute_strategy(
                &mut TransactionStrategy {
                    optimized_tasks: vec![finalize_task],
                    lookup_tables_keys: vec![],
                    compressed: task.is_compressed(),
                },
                &None::<IntentPersisterImpl>,
                None::<CommitSlotFn>,
            )
            .await
            .map_err(IntentExecutorError::FailedFinalizePreparationError)?
            .map_err(|err| IntentExecutorError::FailedToFinalizeError {
                err,
                commit_signature: None,
                finalize_signature: *failed_signature,
            })?;

        Ok(ControlFlow::Continue(TransactionStrategy {
            optimized_tasks: vec![],
            lookup_tables_keys: vec![],
            compressed: task.is_compressed(),
        }))
    }
}
