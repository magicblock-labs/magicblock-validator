use std::ops::ControlFlow;

use log::{error, warn};
use solana_pubkey::Pubkey;
use solana_sdk::signature::Signature;

use crate::{
    intent_executor::{
        error::{
            IntentExecutorError, IntentExecutorResult,
            TransactionStrategyExecutionError,
        },
        task_info_fetcher::TaskInfoFetcher,
        two_stage_executor::sealed::Sealed,
        IntentExecutorImpl,
    },
    persist::IntentPersister,
    tasks::task_strategist::TransactionStrategy,
    transaction_preparator::TransactionPreparator,
};

pub struct Initialized {
    /// Commit stage strategy
    pub commit_strategy: TransactionStrategy,
    /// Finalize stage strategy
    pub finalize_strategy: TransactionStrategy,
}

pub struct Committed {
    /// Signature of commit stage
    pub commit_signature: Signature,
    /// Finalize stage strategy
    pub finalize_strategy: TransactionStrategy,
}

pub struct Finalized {
    /// Signature of commit stage
    pub commit_signature: Signature,
    /// Signature of finalize stage
    pub finalize_signature: Signature,
}

pub struct TwoStageExecutor<'a, T, F, S: Sealed> {
    pub(in crate::intent_executor) inner: &'a mut IntentExecutorImpl<T, F>,
    pub state: S,
}

impl<'a, T, F> TwoStageExecutor<'a, T, F, Initialized>
where
    T: TransactionPreparator,
    F: TaskInfoFetcher,
{
    const RECURSION_CEILING: u8 = 10;

    pub fn new(
        executor: &'a mut IntentExecutorImpl<T, F>,
        commit_strategy: TransactionStrategy,
        finalize_strategy: TransactionStrategy,
    ) -> Self {
        Self {
            inner: executor,
            state: Initialized {
                commit_strategy,
                finalize_strategy,
            },
        }
    }

    pub async fn commit<P: IntentPersister>(
        mut self,
        committed_pubkeys: &[Pubkey],
        persister: &Option<P>,
    ) -> IntentExecutorResult<TwoStageExecutor<'a, T, F, Committed>> {
        let mut i = 0;
        let commit_result = loop {
            i += 1;

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

            let flow = Self::patch_commit_strategy(
                self.inner,
                &execution_err,
                &mut self.state.commit_strategy,
                committed_pubkeys,
            )
            .await?;
            let cleanup = match flow {
                ControlFlow::Continue(value) => value,
                ControlFlow::Break(()) => {
                    break Err(execution_err);
                }
            };
            self.inner.junk.push(cleanup);

            if i >= Self::RECURSION_CEILING {
                error!(
                    "CRITICAL! Recursion ceiling reached in intent execution."
                );
                break Err(execution_err);
            } else {
                self.inner.patched_errors.push(execution_err);
            }
        };

        // Even if failed - dump commit into junk
        self.inner.junk.push(self.state.commit_strategy);
        let commit_signature = commit_result.map_err(|err| {
            IntentExecutorError::from_commit_execution_error(err)
        })?;

        let state = Committed {
            commit_signature,
            finalize_strategy: self.state.finalize_strategy,
        };
        Ok(TwoStageExecutor {
            inner: self.inner,
            state,
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
        inner: &IntentExecutorImpl<T, F>,
        err: &TransactionStrategyExecutionError,
        commit_strategy: &mut TransactionStrategy,
        committed_pubkeys: &[Pubkey],
    ) -> IntentExecutorResult<ControlFlow<(), TransactionStrategy>> {
        match err {
            TransactionStrategyExecutionError::CommitIDError(_, _) => {
                let to_cleanup = inner
                    .handle_commit_id_error(committed_pubkeys, commit_strategy)
                    .await?;
                Ok(ControlFlow::Continue(to_cleanup))
            }
            TransactionStrategyExecutionError::UnfinalizedAccountError(
                _,
                _,
            ) => {
                todo!()
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
}

impl<'a, T, F> TwoStageExecutor<'a, T, F, Committed>
where
    T: TransactionPreparator,
    F: TaskInfoFetcher,
{
    const RECURSION_CEILING: u8 = 10;

    pub async fn finalize<P: IntentPersister>(
        mut self,
        persister: &Option<P>,
    ) -> IntentExecutorResult<TwoStageExecutor<'a, T, F, Finalized>> {
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

            let flow = Self::patch_finalize_strategy(
                self.inner,
                &execution_err,
                &mut self.state.finalize_strategy,
            )
            .await?;

            let cleanup = match flow {
                ControlFlow::Continue(cleanup) => cleanup,
                ControlFlow::Break(()) => {
                    break Err(execution_err);
                }
            };
            self.inner.junk.push(cleanup);

            if i >= Self::RECURSION_CEILING {
                error!(
                    "CRITICAL! Recursion ceiling reached in intent execution."
                );
                break Err(execution_err);
            } else {
                self.inner.patched_errors.push(execution_err);
            }
        };

        // Even if failed - dump finalize into junk
        self.inner.junk.push(self.state.finalize_strategy);
        let finalize_signature = finalize_result.map_err(|err| {
            IntentExecutorError::from_finalize_execution_error(
                err,
                Some(self.state.commit_signature),
            )
        })?;

        let finalized = Finalized {
            commit_signature: self.state.commit_signature,
            finalize_signature,
        };
        Ok(TwoStageExecutor {
            inner: self.inner,
            state: finalized,
        })
    }

    /// Patches Finalize stage `transaction_strategy` in response to a recoverable
    /// [`TransactionStrategyExecutionError`], optionally preparing cleanup data
    /// to be applied after a retry.
    ///
    /// [`TransactionStrategyExecutionError`], returning either:
    /// - `Continue(to_cleanup)` when a retry should be attempted with cleanup metadata, or
    /// - `Break(())` when this stage cannot be recovered.
    pub async fn patch_finalize_strategy(
        inner: &IntentExecutorImpl<T, F>,
        err: &TransactionStrategyExecutionError,
        finalize_strategy: &mut TransactionStrategy,
    ) -> IntentExecutorResult<ControlFlow<(), TransactionStrategy>> {
        match err {
            TransactionStrategyExecutionError::CommitIDError(_, _) => {
                // Unexpected error in Two Stage commit
                error!("Unexpected error in two stage finalize flow: {}", err);
                Ok(ControlFlow::Break(()))
            }
            TransactionStrategyExecutionError::ActionsError(_, _) => {
                // Here we patch strategy for it to be retried in next iteration
                // & we also record data that has to be cleaned up after patch
                let to_cleanup = inner.handle_actions_error(finalize_strategy);
                Ok(ControlFlow::Continue(to_cleanup))
            }
            TransactionStrategyExecutionError::UnfinalizedAccountError(
                _,
                _,
            ) => {
                todo!()
            }
            TransactionStrategyExecutionError::UndelegationError(_, _) => {
                // Here we patch strategy for it to be retried in next iteration
                // & we also record data that has to be cleaned up after patch
                let to_cleanup =
                    inner.handle_undelegation_error(finalize_strategy);
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

mod sealed {
    pub trait Sealed {}

    impl Sealed for super::Initialized {}
    impl Sealed for super::Committed {}
    impl Sealed for super::Finalized {}
}
