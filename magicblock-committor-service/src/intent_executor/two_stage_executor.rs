use std::{ops::ControlFlow, sync::Arc};

use solana_commitment_config::CommitmentConfig;
use solana_pubkey::Pubkey;
use solana_rpc_client_api::config::RpcTransactionConfig;
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
        CommitSlotFn, IntentExecutorImpl,
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

    #[instrument(
        skip(self, committed_pubkeys, persister),
        fields(stage = "commit")
    )]
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
                    None::<CommitSlotFn>,
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
                error!("CRITICAL! Recursion ceiling reached");
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
            err
            @ TransactionStrategyExecutionError::UnfinalizedAccountError(
                _,
                signature,
            ) => {
                let optimized_tasks =
                    commit_strategy.optimized_tasks.as_slice();
                let task_index = err.task_index();
                if let Some(task) = task_index
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
                        task_index = ?task_index,
                        optimized_tasks_len = optimized_tasks.len(),
                        error = ?err,
                        "RPC returned unexpected task index"
                    );
                    Ok(ControlFlow::Break(()))
                }
            }
            TransactionStrategyExecutionError::ActionsError(_, _) => {
                // Unexpected in Two Stage commit
                // That would mean that Two Stage executes Standalone commit
                error!(error = ?err, "Unexpected error in two stage commit flow");
                Ok(ControlFlow::Break(()))
            }
            TransactionStrategyExecutionError::UndelegationError(_, _) => {
                // Unexpected in Two Stage commit
                // That would mean that Two Stage executes undelegation in commit phase
                error!(error = ?err, "Unexpected error in two stage commit flow");
                Ok(ControlFlow::Break(()))
            }
            TransactionStrategyExecutionError::CpiLimitError(_, _) => {
                // Can't be handled
                error!(error = ?err, "Commit tasks exceeded CpiLimitError");
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
                None,
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
            compressed: task.is_compressed(),
        }))
    }
}

impl<'a, T, F> TwoStageExecutor<'a, T, F, Committed>
where
    T: TransactionPreparator,
    F: TaskInfoFetcher,
{
    const RECURSION_CEILING: u8 = 10;

    #[instrument(skip(self, persister), fields(stage = "finalize"))]
    pub async fn finalize<P: IntentPersister>(
        mut self,
        persister: &Option<P>,
    ) -> IntentExecutorResult<TwoStageExecutor<'a, T, F, Finalized>> {
        // Fetching the slot at which the transaction was executed
        // Task preparations requiring fresh data can use that info
        // Using a future to avoid fetching when unnecessary
        let rpc_client = self.inner.rpc_client.clone();
        let sig = self.state.commit_signature;
        let commit_slot_fn: CommitSlotFn = Arc::new(move || {
            let rpc_client = rpc_client.clone();
            Box::pin(async move {
                rpc_client
                    .get_transaction(
                        &sig,
                        Some(RpcTransactionConfig {
                            commitment: Some(CommitmentConfig::confirmed()),
                            max_supported_transaction_version: Some(0),
                            ..Default::default()
                        }),
                    )
                    .await
                    .inspect_err(|err| {
                        error!("Failed to get commit slot: {}", err)
                    })
                    .map(|tx| tx.slot)
                    .ok()
            })
        });

        let mut i = 0;
        let finalize_result = loop {
            i += 1;

            // Prepare & execute message
            let execution_result = self
                .inner
                .prepare_and_execute_strategy(
                    &mut self.state.finalize_strategy,
                    persister,
                    Some(commit_slot_fn.clone()),
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
                error!("CRITICAL! Recursion ceiling reached");
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
                let to_cleanup = inner.handle_actions_error(finalize_strategy);
                Ok(ControlFlow::Continue(to_cleanup))
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
                warn!(error = ?err, "Finalization tasks exceeded CpiLimitError");
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
