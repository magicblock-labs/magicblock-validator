use std::{mem, ops::ControlFlow};

use solana_pubkey::Pubkey;
use solana_signature::Signature;
use tracing::{error, instrument, warn};

use crate::{
    intent_executor::{
        error::{
            IntentExecutorError, IntentExecutorResult,
            TransactionStrategyExecutionError,
        },
        task_info_fetcher::TaskInfoFetcher,
        two_stage_executor::sealed::Sealed,
        ActionsCallbackExecutor, IntentExecutorImpl,
    },
    persist::{IntentPersister, IntentPersisterImpl},
    tasks::{
        commit_task::CommitTask, task_strategist::TransactionStrategy,
        BaseTaskImpl, FinalizeTask,
    },
    transaction_preparator::TransactionPreparator,
};

pub struct Initialized {
    /// Commit stage strategy
    pub commit_strategy: TransactionStrategy,
    /// Finalize stage strategy
    pub finalize_strategy: TransactionStrategy,

    current_attempt: u8,
}

pub struct Committed {
    /// Signature of commit stage
    pub commit_signature: Signature,
    /// Finalize stage strategy
    pub finalize_strategy: TransactionStrategy,

    current_attempt: u8,
}

pub struct Finalized {
    /// Signature of commit stage
    pub commit_signature: Signature,
    /// Signature of finalize stage
    pub finalize_signature: Signature,
}

pub struct TwoStageExecutor<'a, T, F, A, S: Sealed> {
    // TODO(edwin): remove this and replace with IntentClient
    pub(in crate::intent_executor) inner: &'a mut IntentExecutorImpl<T, F, A>,
    pub state: S,
}

impl<'a, T, F, A> TwoStageExecutor<'a, T, F, A, Initialized>
where
    T: TransactionPreparator,
    F: TaskInfoFetcher,
    A: ActionsCallbackExecutor,
{
    const RECURSION_CEILING: u8 = 10;

    pub fn new(
        executor: &'a mut IntentExecutorImpl<T, F, A>,
        commit_strategy: TransactionStrategy,
        finalize_strategy: TransactionStrategy,
    ) -> Self {
        Self {
            inner: executor,
            state: Initialized {
                commit_strategy,
                finalize_strategy,
                current_attempt: 0,
            },
        }
    }

    #[instrument(
        skip(self, committed_pubkeys, persister),
        fields(stage = "commit")
    )]
    pub async fn commit<P: IntentPersister>(
        &mut self,
        committed_pubkeys: &[Pubkey],
        persister: &Option<P>,
    ) -> IntentExecutorResult<Signature> {
        let commit_result = loop {
            self.state.current_attempt += 1;

            // Prepare & execute message
            let execution_result = self
                .inner
                .prepare_and_execute_strategy(
                    &mut self.state.commit_strategy,
                    persister,
                )
                .await
                .map_err(IntentExecutorError::FailedCommitPreparationError)?;
            let execution_err = match execution_result {
                Ok(value) => break Ok(value),
                Err(err) => err,
            };

            let flow = self
                .patch_commit_strategy(&execution_err, committed_pubkeys)
                .await?;
            let cleanup = match flow {
                ControlFlow::Continue(value) => value,
                ControlFlow::Break(()) => {
                    break Err(execution_err);
                }
            };
            self.inner.junk.push(cleanup);

            if self.state.current_attempt >= Self::RECURSION_CEILING {
                error!("CRITICAL! Recursion ceiling reached");
                break Err(execution_err);
            } else {
                self.inner.patched_errors.push(execution_err);
            }
        };

        // Even if failed - dump commit into junk
        self.inner
            .junk
            .push(mem::take(&mut self.state.commit_strategy));
        commit_result.map_err(|err| {
            IntentExecutorError::from_commit_execution_error(err)
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
        &mut self,
        err: &TransactionStrategyExecutionError,
        committed_pubkeys: &[Pubkey],
    ) -> IntentExecutorResult<ControlFlow<(), TransactionStrategy>> {
        match err {
            TransactionStrategyExecutionError::CommitIDError(_, _) => {
                let to_cleanup = self.inner
                    .handle_commit_id_error(committed_pubkeys, &mut self.state.commit_strategy)
                    .await?;
                Ok(ControlFlow::Continue(to_cleanup))
            }
            err
            @ TransactionStrategyExecutionError::UnfinalizedAccountError(
                _,
                signature,
            ) => {
                let optimized_tasks =
                    self.state.commit_strategy.optimized_tasks.as_slice();
                let task_index = err.task_index();
                if let Some(BaseTaskImpl::Commit(task)) = task_index
                    .and_then(|index| optimized_tasks.get(index as usize))
                {
                    Self::handle_unfinalized_account_error(
                        self.inner, signature, task,
                    )
                    .await
                } else {
                    error!(
                        task_index = ?task_index,
                        optimized_tasks_len = optimized_tasks.len(),
                        error = ?err,
                        "RPC returned unexpected task index"
                    );
                    Ok(ControlFlow::Break(()))
                }
            }
            TransactionStrategyExecutionError::ActionsError(_, _) => {
                // Intent bundles allow for actions to be in commit stage
                let to_cleanup = handle_actions_error(self.inner, &mut self.state.commit_strategy);
                Ok(ControlFlow::Continue(to_cleanup))
            }
            TransactionStrategyExecutionError::UndelegationError(_, _) => {
                // Unexpected in Two Stage commit
                // That would mean that Two Stage executes undelegation in commit phase
                error!(error = ?err, "Unexpected error in two stage commit flow");
                Ok(ControlFlow::Break(()))
            }
            TransactionStrategyExecutionError::CpiLimitError(_, _)
            | TransactionStrategyExecutionError::LoadedAccountsDataSizeExceeded(
                _,
                _,
            ) => {
                // Can't be handled
                error!(error = ?err, "Commit tasks exceeded execution limit");
                Ok(ControlFlow::Break(()))
            }
            TransactionStrategyExecutionError::InternalError(_) => {
                // Can't be handled
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
            .map_err(IntentExecutorError::FailedCommitPreparationError)?
            .map_err(|err| IntentExecutorError::FailedToCommitError {
                err,
                signature: *failed_signature,
            })?;

        Ok(ControlFlow::Continue(TransactionStrategy {
            optimized_tasks: vec![],
            lookup_tables_keys: vec![],
        }))
    }

    /// Removes actions from commit & finalize strategies
    /// Executes callbacks
    pub fn execute_callbacks(&mut self) {
        let junk_strategy =
            handle_actions_error(&self.inner, &mut self.state.commit_strategy);
        self.inner.junk.push(junk_strategy);

        let junk_strategy = handle_actions_error(
            &self.inner,
            &mut self.state.finalize_strategy,
        );
        self.inner.junk.push(junk_strategy);
    }

    /// Transactions to next executor state
    pub fn done(
        self,
        commit_signature: Signature,
    ) -> TwoStageExecutor<'a, T, F, A, Committed> {
        TwoStageExecutor {
            inner: self.inner,
            state: Committed {
                commit_signature,
                finalize_strategy: self.state.finalize_strategy,
                current_attempt: 0,
            },
        }
    }
}

impl<'a, T, F, A> TwoStageExecutor<'a, T, F, A, Committed>
where
    T: TransactionPreparator,
    F: TaskInfoFetcher,
    A: ActionsCallbackExecutor,
{
    const RECURSION_CEILING: u8 = 10;

    #[instrument(skip(self, persister), fields(stage = "finalize"))]
    pub async fn finalize<P: IntentPersister>(
        &mut self,
        persister: &Option<P>,
    ) -> IntentExecutorResult<Signature> {
        let mut i = 0;
        let finalize_result = loop {
            i += 1;

            // Prepare & execute message
            let execution_result = self
                .inner
                .prepare_and_execute_strategy(
                    &mut self.state.finalize_strategy,
                    persister,
                )
                .await
                .map_err(IntentExecutorError::FailedFinalizePreparationError)?;
            let execution_err = match execution_result {
                Ok(value) => break Ok(value),
                Err(err) => err,
            };

            let flow = self.patch_finalize_strategy(&execution_err).await?;

            let cleanup = match flow {
                ControlFlow::Continue(cleanup) => cleanup,
                ControlFlow::Break(()) => {
                    break Err(execution_err);
                }
            };
            self.inner.junk.push(cleanup);

            if i >= Self::RECURSION_CEILING {
                error!("CRITICAL! Recursion ceiling reached");
                break Err(execution_err);
            } else {
                self.inner.patched_errors.push(execution_err);
            }
        };

        // Even if failed - dump finalize into junk
        self.inner
            .junk
            .push(mem::take(&mut self.state.finalize_strategy));
        finalize_result.map_err(|err| {
            IntentExecutorError::from_finalize_execution_error(
                err,
                Some(self.state.commit_signature),
            )
        })
    }

    /// Removes actions from finalize strateg
    /// Executes callbacks
    pub fn execute_callbacks(&mut self) {
        let junk_strategy =
            handle_actions_error(self.inner, &mut self.state.finalize_strategy);
        self.inner.junk.push(junk_strategy);
    }

    /// Patches Finalize stage `transaction_strategy` in response to a recoverable
    /// [`TransactionStrategyExecutionError`], optionally preparing cleanup data
    /// to be applied after a retry.
    ///
    /// [`TransactionStrategyExecutionError`], returning either:
    /// - `Continue(to_cleanup)` when a retry should be attempted with cleanup metadata, or
    /// - `Break(())` when this stage cannot be recovered.
    pub async fn patch_finalize_strategy(
        &mut self,
        err: &TransactionStrategyExecutionError,
    ) -> IntentExecutorResult<ControlFlow<(), TransactionStrategy>> {
        match err {
            TransactionStrategyExecutionError::CommitIDError(_, _)
            | TransactionStrategyExecutionError::UnfinalizedAccountError(
                _,
                _,
            ) => {
                // Unexpected error in Two Stage commit
                error!(error = ?err, "Unexpected error in two stage finalize flow");
                Ok(ControlFlow::Break(()))
            }
            TransactionStrategyExecutionError::ActionsError(_, _) => {
                // Here we patch strategy for it to be retried in next iteration
                // & we also record data that has to be cleaned up after patch
                let to_cleanup = handle_actions_error(self.inner, &mut self.state.finalize_strategy);
                Ok(ControlFlow::Continue(to_cleanup))
            }
            TransactionStrategyExecutionError::UndelegationError(_, _) => {
                // Here we patch strategy for it to be retried in next iteration
                // & we also record data that has to be cleaned up after patch
                let to_cleanup =
                    self.inner.handle_undelegation_error(&mut self.state.finalize_strategy);
                Ok(ControlFlow::Continue(to_cleanup))
            }
            TransactionStrategyExecutionError::CpiLimitError(_, _)
            | TransactionStrategyExecutionError::LoadedAccountsDataSizeExceeded(_, _) => {
                // Can't be handled
                warn!(error = ?err, "Finalization tasks exceeded execution limit");
                Ok(ControlFlow::Break(()))
            }
            TransactionStrategyExecutionError::InternalError(_) => {
                // Can't be handled
                Ok(ControlFlow::Break(()))
            }
        }
    }

    pub fn done(
        self,
        finalize_signature: Signature,
    ) -> TwoStageExecutor<'a, T, F, A, Finalized> {
        let finalized = Finalized {
            commit_signature: self.state.commit_signature,
            finalize_signature,
        };
        TwoStageExecutor {
            inner: self.inner,
            state: finalized,
        }
    }
}

/// Removes actions from strategy and if they contained callbacks executes them
/// Returns removed strategy
fn handle_actions_error<T, F, A>(
    inner: &IntentExecutorImpl<T, F, A>,
    transaction_strategy: &mut TransactionStrategy,
) -> TransactionStrategy
where
    T: TransactionPreparator,
    F: TaskInfoFetcher,
    A: ActionsCallbackExecutor,
{
    let mut removed_actions = inner.remove_actions(transaction_strategy);
    let callbacks = removed_actions.extract_action_callbacks();
    if !callbacks.is_empty() {
        inner.actions_callback_executor.execute(callbacks);
    }

    removed_actions
}

mod sealed {
    pub trait Sealed {}

    impl Sealed for super::Initialized {}
    impl Sealed for super::Committed {}
    impl Sealed for super::Finalized {}
}
