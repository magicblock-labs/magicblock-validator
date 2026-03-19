use std::{mem, ops::ControlFlow};

use magicblock_core::traits::{
    ActionError, ActionResult, ActionsCallbackScheduler,
};
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_signature::Signature;
use solana_signer::Signer;
use tracing::{error, instrument, warn};

use crate::{
    intent_executor::{
        error::{
            IntentExecutorError, IntentExecutorResult,
            TransactionStrategyExecutionError,
        },
        intent_execution_client::IntentExecutionClient,
        task_info_fetcher::{CacheTaskInfoFetcher, TaskInfoFetcher},
        two_stage_executor::sealed::Sealed,
        utils::{handle_commit_id_error, handle_undelegation_error},
        IntentExecutionReport,
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
    commit_strategy: TransactionStrategy,
    /// Finalize stage strategy
    finalize_strategy: TransactionStrategy,

    current_attempt: u8,
}

pub struct Committed {
    /// Signature of commit stage
    commit_signature: Signature,
    /// Finalize stage strategy
    finalize_strategy: TransactionStrategy,

    current_attempt: u8,
}

pub struct Finalized {
    /// Signature of commit stage
    pub commit_signature: Signature,
    /// Signature of finalize stage
    pub finalize_signature: Signature,
}

pub struct TwoStageExecutor<'a, A, S: Sealed> {
    state: S,
    authority: Keypair,
    intent_client: IntentExecutionClient,
    callback_scheduler: A,
    execution_report: &'a mut IntentExecutionReport,
}

impl<'a, A> TwoStageExecutor<'a, A, Initialized>
where
    A: ActionsCallbackScheduler,
{
    const RECURSION_CEILING: u8 = 10;

    pub fn new(
        authority: Keypair,
        commit_strategy: TransactionStrategy,
        finalize_strategy: TransactionStrategy,
        intent_client: IntentExecutionClient,
        callback_scheduler: A,
        execution_report: &'a mut IntentExecutionReport,
    ) -> Self {
        Self {
            authority,
            intent_client,
            execution_report,
            callback_scheduler,
            state: Initialized {
                commit_strategy,
                finalize_strategy,
                current_attempt: 0,
            },
        }
    }

    #[instrument(
        skip(
            self,
            committed_pubkeys,
            transaction_preparator,
            task_info_fetcher,
            persister
        ),
        fields(stage = "commit")
    )]
    pub async fn commit<T, F, P>(
        &mut self,
        committed_pubkeys: &[Pubkey],
        transaction_preparator: &T,
        task_info_fetcher: &CacheTaskInfoFetcher<F>,
        persister: &Option<P>,
    ) -> IntentExecutorResult<Signature>
    where
        T: TransactionPreparator,
        F: TaskInfoFetcher,
        P: IntentPersister,
    {
        let commit_result = loop {
            self.state.current_attempt += 1;

            // Prepare & execute message
            let execution_result = self
                .intent_client
                .prepare_and_execute_strategy(
                    &self.authority,
                    transaction_preparator,
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
                .patch_commit_strategy(
                    &execution_err,
                    task_info_fetcher,
                    transaction_preparator,
                    committed_pubkeys,
                )
                .await?;
            let cleanup = match flow {
                ControlFlow::Continue(value) => value,
                ControlFlow::Break(()) => {
                    break Err(execution_err);
                }
            };
            self.execution_report.dispose(cleanup);

            if self.state.current_attempt >= Self::RECURSION_CEILING {
                error!("CRITICAL! Recursion ceiling reached");
                break Err(execution_err);
            } else {
                self.execution_report.add_patched_error(execution_err);
            }
        };

        self.execute_callbacks(commit_result.as_ref().map(|_| ()));
        self.execution_report
            .dispose(mem::take(&mut self.state.commit_strategy));
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
    pub async fn patch_commit_strategy<T, F>(
        &mut self,
        err: &TransactionStrategyExecutionError,
        task_info_fetcher: &CacheTaskInfoFetcher<F>,
        transaction_preparator: &T,
        committed_pubkeys: &[Pubkey],
    ) -> IntentExecutorResult<ControlFlow<(), TransactionStrategy>>
    where
        T: TransactionPreparator,
        F: TaskInfoFetcher,
    {
        match err {
            TransactionStrategyExecutionError::CommitIDError(_, _) => {
                let to_cleanup = handle_commit_id_error(
                    &self.authority.pubkey(),
                    task_info_fetcher,
                    committed_pubkeys,
                    &mut self.state.commit_strategy
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
                    self.state.commit_strategy.optimized_tasks.as_slice();
                let task_index = err.task_index();
                if let Some(BaseTaskImpl::Commit(task)) = task_index
                    .and_then(|index| optimized_tasks.get(index as usize))
                {
                    self.handle_unfinalized_account_error(signature, task, transaction_preparator)
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
            TransactionStrategyExecutionError::ActionsError(err, signature) => {
                // Intent bundles allow for actions to be in commit stage
                let action_error = Err(ActionError::ActionsError(err.clone(), *signature));
                let to_cleanup = handle_actions_result(
                    &self.authority.pubkey(),
                    &self.callback_scheduler,
                    &mut self.execution_report,
                    &mut self.state.commit_strategy,
                    action_error
                );
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
    async fn handle_unfinalized_account_error<T: TransactionPreparator>(
        &self,
        failed_signature: &Option<Signature>,
        task: &CommitTask,
        transaction_preparator: &T,
    ) -> IntentExecutorResult<ControlFlow<(), TransactionStrategy>> {
        let finalize_task: BaseTaskImpl = FinalizeTask {
            delegated_account: task.committed_account.pubkey,
        }
        .into();
        self.intent_client
            .prepare_and_execute_strategy(
                &self.authority,
                transaction_preparator,
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

    /// On `Err`: removes actions from both commit and finalize strategies and
    /// executes all their callbacks with the error.
    /// On `Ok`: removes actions only from commit strategy and executes their
    /// callbacks, preserving finalize-stage actions for the finalize phase.
    pub fn execute_callbacks(
        &mut self,
        result: Result<(), impl Into<ActionError>>,
    ) {
        let result = result.map(|_| ()).map_err(|err| err.into());
        let junk_strategy = handle_actions_result(
            &self.authority.pubkey(),
            &self.callback_scheduler,
            &mut self.execution_report,
            &mut self.state.commit_strategy,
            result.clone(),
        );
        self.execution_report.dispose(junk_strategy);

        if result.is_err() {
            let junk_strategy = handle_actions_result(
                &self.authority.pubkey(),
                &self.callback_scheduler,
                &mut self.execution_report,
                &mut self.state.finalize_strategy,
                result,
            );
            self.execution_report.dispose(junk_strategy);
        }
    }

    /// Transitions to next executor state
    pub fn done(
        self,
        commit_signature: Signature,
    ) -> TwoStageExecutor<'a, A, Committed> {
        TwoStageExecutor {
            authority: self.authority,
            intent_client: self.intent_client,
            callback_scheduler: self.callback_scheduler,
            execution_report: self.execution_report,
            state: Committed {
                commit_signature,
                finalize_strategy: self.state.finalize_strategy,
                current_attempt: 0,
            },
        }
    }
}

impl<'a, A> TwoStageExecutor<'a, A, Committed>
where
    A: ActionsCallbackScheduler,
{
    const RECURSION_CEILING: u8 = 10;

    #[instrument(
        skip(self, transaction_preparator, persister),
        fields(stage = "finalize")
    )]
    pub async fn finalize<T, P>(
        &mut self,
        transaction_preparator: &T,
        persister: &Option<P>,
    ) -> IntentExecutorResult<Signature>
    where
        T: TransactionPreparator,
        P: IntentPersister,
    {
        let finalize_result = loop {
            self.state.current_attempt += 1;

            // Prepare & execute message
            let execution_result = self
                .intent_client
                .prepare_and_execute_strategy(
                    &self.authority,
                    transaction_preparator,
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
            self.execution_report.dispose(cleanup);

            if self.state.current_attempt >= Self::RECURSION_CEILING {
                error!("CRITICAL! Recursion ceiling reached");
                break Err(execution_err);
            } else {
                self.execution_report.add_patched_error(execution_err);
            }
        };

        // Even if failed - dump finalize into junk
        self.execute_callbacks(finalize_result.as_ref().map(|_| ()));
        self.execution_report
            .dispose(mem::take(&mut self.state.finalize_strategy));
        finalize_result.map_err(|err| {
            IntentExecutorError::from_finalize_execution_error(
                err,
                Some(self.state.commit_signature),
            )
        })
    }

    /// Removes actions from finalize strateg
    /// Executes callbacks
    pub fn execute_callbacks(
        &mut self,
        result: Result<(), impl Into<ActionError>>,
    ) {
        let junk_strategy = handle_actions_result(
            &self.authority.pubkey(),
            &self.callback_scheduler,
            &mut self.execution_report,
            &mut self.state.finalize_strategy,
            result.map_err(|err| err.into()),
        );
        self.execution_report.dispose(junk_strategy);
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
            TransactionStrategyExecutionError::ActionsError(err, signature) => {
                // Here we patch strategy for it to be retried in next iteration
                // & we also record data that has to be cleaned up after patch
                let action_error = Err(ActionError::ActionsError(err.clone(), *signature));
                let to_cleanup = handle_actions_result(
                    &self.authority.pubkey(),
                    &self.callback_scheduler,
                    &mut self.execution_report,
                    &mut self.state.finalize_strategy,
                    action_error
                );
                Ok(ControlFlow::Continue(to_cleanup))
            }
            TransactionStrategyExecutionError::UndelegationError(_, _) => {
                // Here we patch strategy for it to be retried in next iteration
                // & we also record data that has to be cleaned up after patch
                let to_cleanup =
                    handle_undelegation_error(&self.authority.pubkey(), &mut self.state.finalize_strategy);
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

    /// Transitions to next executor state
    pub fn done(
        self,
        finalize_signature: Signature,
    ) -> TwoStageExecutor<'a, A, Finalized> {
        let finalized = Finalized {
            commit_signature: self.state.commit_signature,
            finalize_signature,
        };
        TwoStageExecutor {
            authority: self.authority,
            intent_client: self.intent_client,
            callback_scheduler: self.callback_scheduler,
            execution_report: self.execution_report,
            state: finalized,
        }
    }
}

impl<'a, A> TwoStageExecutor<'a, A, Finalized> {
    pub fn result(self) -> Finalized {
        self.state
    }
}

fn handle_actions_result<A>(
    authority: &Pubkey,
    callback_scheduler: &A,
    execution_report: &mut IntentExecutionReport,
    transaction_strategy: &mut TransactionStrategy,
    result: ActionResult,
) -> TransactionStrategy
where
    A: ActionsCallbackScheduler,
{
    let mut removed_actions = transaction_strategy.remove_actions(&authority);
    let callbacks = removed_actions.extract_action_callbacks();
    if !callbacks.is_empty() {
        let result = callback_scheduler.schedule(callbacks, result);
        execution_report.add_callback_report(result);
    }

    removed_actions
}

mod sealed {
    pub trait Sealed {}

    impl Sealed for super::Initialized {}
    impl Sealed for super::Committed {}
    impl Sealed for super::Finalized {}
}
