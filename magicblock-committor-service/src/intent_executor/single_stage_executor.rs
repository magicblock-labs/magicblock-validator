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
        ActionsCallbackExecutor, ExecutionOutput, IntentExecutorImpl,
    },
    persist::{IntentPersister, IntentPersisterImpl},
    tasks::{
        commit_task::CommitTask, task_strategist::TransactionStrategy,
        BaseTaskImpl, FinalizeTask,
    },
    transaction_preparator::TransactionPreparator,
};

pub struct SingleStageExecutor<'a, T, F, A> {
    current_attempt: u8,
    // TODO(edwin): remove this and replace with IntentClient
    pub(in crate::intent_executor) inner: &'a mut IntentExecutorImpl<T, F, A>,
    pub transaction_strategy: TransactionStrategy,
}

impl<'a, T, F, A> SingleStageExecutor<'a, T, F, A>
where
    T: TransactionPreparator,
    F: TaskInfoFetcher,
    A: ActionsCallbackExecutor,
{
    pub fn new(
        executor: &'a mut IntentExecutorImpl<T, F, A>,
        transaction_strategy: TransactionStrategy,
    ) -> Self {
        Self {
            current_attempt: 0,
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
        committed_pubkeys: &[Pubkey],
        persister: &Option<P>,
    ) -> IntentExecutorResult<ExecutionOutput> {
        const RECURSION_CEILING: u8 = 10;

        let result = loop {
            self.current_attempt += 1;

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
                    break Ok(ExecutionOutput::SingleStage(value));
                }
                Err(err) => err,
            };

            // Attempt patching
            let flow = self
                .patch_strategy(&execution_err, committed_pubkeys)
                .await?;
            let cleanup = match flow {
                ControlFlow::Continue(cleanup) => cleanup,
                ControlFlow::Break(()) => {
                    break Err(execution_err);
                }
            };
            self.inner.junk.push(cleanup);

            if self.current_attempt >= RECURSION_CEILING {
                error!(
                    attempt = self.current_attempt,
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

    /// Removes actions from strategy and if they contained callbacks executes them
    /// Returns removed strategy
    fn handle_actions_error(&mut self) -> TransactionStrategy {
        let mut removed_actions =
            self.inner.remove_actions(&mut self.transaction_strategy);
        let callbacks = removed_actions.extract_action_callbacks();
        if !callbacks.is_empty() {
            self.inner.actions_callback_executor.execute(callbacks);
        }

        removed_actions
    }

    pub fn execute_callbacks(&mut self) {
        let junk_strategy = self.handle_actions_error();
        self.inner.junk.push(junk_strategy);
    }

    /// Patch the current `transaction_strategy` in response to a recoverable
    /// [`TransactionStrategyExecutionError`], optionally preparing cleanup data
    /// to be applied after a retry.
    ///
    /// [`TransactionStrategyExecutionError`], returning either:
    /// - `Continue(to_cleanup)` when a retry should be attempted with cleanup metadata, or
    /// - `Break(())` when this stage cannot be recovered here.
    pub async fn patch_strategy(
        &mut self,
        err: &TransactionStrategyExecutionError,
        committed_pubkeys: &[Pubkey],
    ) -> IntentExecutorResult<ControlFlow<(), TransactionStrategy>> {
        if committed_pubkeys.is_empty() {
            // No patching is applicable if intent doesn't commit accounts
            return Ok(ControlFlow::Break(()));
        }

        match err {
            TransactionStrategyExecutionError::ActionsError(_, _) => {
                // // Here we patch strategy for it to be retried in next iteration
                // // & we also record data that has to be cleaned up after patch
                let to_cleanup = self.handle_actions_error();
                Ok(ControlFlow::Continue(to_cleanup))
            }
            TransactionStrategyExecutionError::CommitIDError(_, _) => {
                // Here we patch strategy for it to be retried in next iteration
                // & we also record data that has to be cleaned up after patch
                let to_cleanup = self.inner
                    .handle_commit_id_error(
                        committed_pubkeys,
                        &mut self.transaction_strategy,
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
                    self.transaction_strategy.optimized_tasks.as_slice();
                if let Some(BaseTaskImpl::Commit(task)) = err
                    .task_index()
                    .and_then(|index| optimized_tasks.get(index as usize))
                {
                    Self::handle_unfinalized_account_error(
                        self.inner, signature, task,
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
                    self.inner.handle_undelegation_error(&mut self.transaction_strategy);
                Ok(ControlFlow::Continue(to_cleanup))
            }
            TransactionStrategyExecutionError::CpiLimitError(_, _)
            | TransactionStrategyExecutionError::LoadedAccountsDataSizeExceeded(_, _) => {
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
        inner: &IntentExecutorImpl<T, F, A>,
        failed_signature: &Option<Signature>,
        task: &CommitTask,
    ) -> IntentExecutorResult<ControlFlow<(), TransactionStrategy>> {
        let finalize_task: BaseTaskImpl = FinalizeTask {
            delegated_account: task.committed_account.pubkey,
        }
        .into();
        inner
            .prepare_and_execute_strategy(
                &mut TransactionStrategy {
                    optimized_tasks: vec![finalize_task],
                    lookup_tables_keys: vec![],
                },
                &None::<IntentPersisterImpl>,
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
        }))
    }
}
