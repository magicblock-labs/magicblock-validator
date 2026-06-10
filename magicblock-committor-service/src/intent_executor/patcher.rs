use std::ops::ControlFlow;

use async_trait::async_trait;
use magicblock_core::traits::{ActionError, ActionsCallbackScheduler};
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_signature::Signature;
use solana_signer::Signer;
use tracing::{error, warn};

use crate::{
    intent_executor::{
        error::{
            IntentExecutorError, IntentExecutorResult,
            TransactionStrategyExecutionError,
        },
        intent_execution_client::IntentExecutionClient,
        task_info_fetcher::{CacheTaskInfoFetcher, TaskInfoFetcher},
        utils::{
            handle_actions_result, handle_commit_id_error,
            handle_undelegation_error, prepare_and_execute_strategy,
        },
        IntentExecutionReport,
    },
    tasks::{task_strategist::TransactionStrategy, BaseTaskImpl, FinalizeTask},
    transaction_preparator::TransactionPreparator,
};

#[async_trait]
pub(in crate::intent_executor) trait Patcher {
    async fn patch(
        &mut self,
        err: &TransactionStrategyExecutionError,
        strategy: &mut TransactionStrategy,
        report: &mut IntentExecutionReport,
    ) -> IntentExecutorResult<ControlFlow<(), TransactionStrategy>>;
}

// --- SingleStagePatcher ---

pub(in crate::intent_executor) struct SingleStagePatcher<'a, F, A, T> {
    pub authority: &'a Keypair,
    pub intent_client: &'a IntentExecutionClient,
    pub callback_scheduler: &'a A,
    pub task_info_fetcher: &'a CacheTaskInfoFetcher<F>,
    pub committed_pubkeys: &'a [Pubkey],
    pub transaction_preparator: &'a T,
}

impl<F, A, T> SingleStagePatcher<'_, F, A, T>
where
    T: TransactionPreparator,
{
    /// Handles unfinalized account error for single stage execution.
    /// Sends a separate tx to finalize account and then continues execution.
    async fn handle_unfinalized_account_error(
        &self,
        failed_signature: &Option<Signature>,
        delegated_account: Pubkey,
    ) -> IntentExecutorResult<ControlFlow<(), TransactionStrategy>> {
        let finalize_task: BaseTaskImpl =
            FinalizeTask { delegated_account }.into();
        prepare_and_execute_strategy(
            self.intent_client,
            self.authority,
            self.transaction_preparator,
            &mut TransactionStrategy {
                optimized_tasks: vec![finalize_task],
                lookup_tables_keys: vec![],
                standalone_action_nonce: None,
            },
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
            standalone_action_nonce: None,
        }))
    }
}

#[async_trait]
impl<F, A, T> Patcher for SingleStagePatcher<'_, F, A, T>
where
    F: TaskInfoFetcher,
    A: ActionsCallbackScheduler,
    T: TransactionPreparator,
{
    /// Patch the current `transaction_strategy` in response to a recoverable
    /// [`TransactionStrategyExecutionError`], optionally preparing cleanup data
    /// to be applied after a retry.
    ///
    /// [`TransactionStrategyExecutionError`], returning either:
    /// - `Continue(to_cleanup)` when a retry should be attempted with cleanup metadata, or
    /// - `Break(())` when this stage cannot be recovered here.
    async fn patch(
        &mut self,
        err: &TransactionStrategyExecutionError,
        strategy: &mut TransactionStrategy,
        report: &mut IntentExecutionReport,
    ) -> IntentExecutorResult<ControlFlow<(), TransactionStrategy>> {
        if self.committed_pubkeys.is_empty() {
            // No patching is applicable if intent doesn't commit accounts
            return Ok(ControlFlow::Break(()));
        }

        match err {
            TransactionStrategyExecutionError::ActionsError(err, signature) => {
                // Here we patch strategy for it to be retried in next iteration
                // & we also record data that has to be cleaned up after patch
                let action_error = Err(ActionError::ActionsError(err.clone(), *signature));
                let to_cleanup = handle_actions_result(
                    &self.authority.pubkey(),
                    self.callback_scheduler,
                    report,
                    strategy,
                    *signature,
                    action_error,
                );
                Ok(ControlFlow::Continue(to_cleanup))
            }
            TransactionStrategyExecutionError::CommitIDError(_, _) => {
                // Here we patch strategy for it to be retried in next iteration
                // & we also record data that has to be cleaned up after patch
                let to_cleanup = handle_commit_id_error(
                    &self.authority.pubkey(),
                    self.task_info_fetcher,
                    self.committed_pubkeys,
                    strategy,
                )
                .await?;
                Ok(ControlFlow::Continue(to_cleanup))
            }
            err @ TransactionStrategyExecutionError::UnfinalizedAccountError(
                _,
                signature,
            ) => {
                let optimized_tasks = strategy.optimized_tasks.as_slice();
                if let Some(delegated_account) = err
                    .task_index()
                    .and_then(|index| optimized_tasks.get(index as usize))
                    .and_then(|task| match task {
                        BaseTaskImpl::Commit(task) => {
                            Some(task.committed_account.pubkey)
                        }
                        BaseTaskImpl::CommitFinalize(task) => {
                            Some(task.committed_account.pubkey)
                        }
                        _ => None,
                    })
                {
                    self.handle_unfinalized_account_error(signature, delegated_account).await
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
                    handle_undelegation_error(&self.authority.pubkey(), strategy);
                Ok(ControlFlow::Continue(to_cleanup))
            }
            TransactionStrategyExecutionError::CpiLimitError(_, _)
            | TransactionStrategyExecutionError::LoadedAccountsDataSizeExceeded(
                _,
                _,
            ) => {
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

// --- CommitStagePatcher ---

pub(in crate::intent_executor) struct CommitStagePatcher<'a, F, A, T> {
    pub authority: &'a Keypair,
    pub intent_client: &'a IntentExecutionClient,
    pub callback_scheduler: &'a A,
    pub task_info_fetcher: &'a CacheTaskInfoFetcher<F>,
    pub committed_pubkeys: &'a [Pubkey],
    pub transaction_preparator: &'a T,
}

#[async_trait]
impl<F, A, T> Patcher for CommitStagePatcher<'_, F, A, T>
where
    F: TaskInfoFetcher,
    A: ActionsCallbackScheduler,
    T: TransactionPreparator,
{
    /// Patches Commit stage `transaction_strategy` in response to a recoverable
    /// [`TransactionStrategyExecutionError`], optionally preparing cleanup data
    /// to be applied after a retry.
    ///
    /// [`TransactionStrategyExecutionError`], returning either:
    /// - `Continue(to_cleanup)` when a retry should be attempted with cleanup metadata, or
    /// - `Break(())` when this stage cannot be recovered.
    async fn patch(
        &mut self,
        err: &TransactionStrategyExecutionError,
        strategy: &mut TransactionStrategy,
        report: &mut IntentExecutionReport,
    ) -> IntentExecutorResult<ControlFlow<(), TransactionStrategy>> {
        match err {
            TransactionStrategyExecutionError::CommitIDError(_, _) => {
                let to_cleanup = handle_commit_id_error(
                    &self.authority.pubkey(),
                    self.task_info_fetcher,
                    self.committed_pubkeys,
                    strategy,
                )
                .await?;
                Ok(ControlFlow::Continue(to_cleanup))
            }
            err @ TransactionStrategyExecutionError::UnfinalizedAccountError(
                _,
                signature,
            ) => {
                let optimized_tasks = strategy.optimized_tasks.as_slice();
                let task_index = err.task_index();
                if let Some(delegated_account) = task_index
                    .and_then(|index| optimized_tasks.get(index as usize))
                    .and_then(|task| match task {
                        BaseTaskImpl::Commit(task) => {
                            Some(task.committed_account.pubkey)
                        }
                        BaseTaskImpl::CommitFinalize(task) => {
                            Some(task.committed_account.pubkey)
                        }
                        _ => None,
                    })
                {
                    self.handle_unfinalized_account_error(signature, delegated_account).await
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
                    self.callback_scheduler,
                    report,
                    strategy,
                    *signature,
                    action_error,
                );
                Ok(ControlFlow::Continue(to_cleanup))
            }
            TransactionStrategyExecutionError::UndelegationError(_, _) => {
                // Unexpected in Two Stage commit
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
}

impl<F, A, T> CommitStagePatcher<'_, F, A, T>
where
    T: TransactionPreparator,
{
    /// Handles unfinalized account error for commit stage execution.
    /// Sends a separate tx to finalize account and then continues execution.
    async fn handle_unfinalized_account_error(
        &self,
        failed_signature: &Option<Signature>,
        delegated_account: Pubkey,
    ) -> IntentExecutorResult<ControlFlow<(), TransactionStrategy>> {
        let finalize_task: BaseTaskImpl =
            FinalizeTask { delegated_account }.into();
        prepare_and_execute_strategy(
            self.intent_client,
            self.authority,
            self.transaction_preparator,
            &mut TransactionStrategy {
                optimized_tasks: vec![finalize_task],
                lookup_tables_keys: vec![],
                standalone_action_nonce: None,
            },
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
            standalone_action_nonce: None,
        }))
    }
}

// --- FinalizeStagePatcher ---

pub(in crate::intent_executor) struct FinalizeStagePatcher<'a, A> {
    pub authority: &'a Keypair,
    pub callback_scheduler: &'a A,
}

#[async_trait]
impl<A> Patcher for FinalizeStagePatcher<'_, A>
where
    A: ActionsCallbackScheduler,
{
    /// Patches Finalize stage `transaction_strategy` in response to a recoverable
    /// [`TransactionStrategyExecutionError`], optionally preparing cleanup data
    /// to be applied after a retry.
    ///
    /// [`TransactionStrategyExecutionError`], returning either:
    /// - `Continue(to_cleanup)` when a retry should be attempted with cleanup metadata, or
    /// - `Break(())` when this stage cannot be recovered.
    async fn patch(
        &mut self,
        err: &TransactionStrategyExecutionError,
        strategy: &mut TransactionStrategy,
        report: &mut IntentExecutionReport,
    ) -> IntentExecutorResult<ControlFlow<(), TransactionStrategy>> {
        match err {
            TransactionStrategyExecutionError::CommitIDError(_, _)
            | TransactionStrategyExecutionError::UnfinalizedAccountError(_, _) => {
                // Unexpected error in Two Stage finalize
                error!(error = ?err, "Unexpected error in two stage finalize flow");
                Ok(ControlFlow::Break(()))
            }
            TransactionStrategyExecutionError::ActionsError(err, signature) => {
                // Here we patch strategy for it to be retried in next iteration
                // & we also record data that has to be cleaned up after patch
                let action_error = Err(ActionError::ActionsError(err.clone(), *signature));
                let to_cleanup = handle_actions_result(
                    &self.authority.pubkey(),
                    self.callback_scheduler,
                    report,
                    strategy,
                    *signature,
                    action_error,
                );
                Ok(ControlFlow::Continue(to_cleanup))
            }
            TransactionStrategyExecutionError::UndelegationError(_, _) => {
                // Here we patch strategy for it to be retried in next iteration
                // & we also record data that has to be cleaned up after patch
                let to_cleanup =
                    handle_undelegation_error(&self.authority.pubkey(), strategy);
                Ok(ControlFlow::Continue(to_cleanup))
            }
            TransactionStrategyExecutionError::CpiLimitError(_, _)
            | TransactionStrategyExecutionError::LoadedAccountsDataSizeExceeded(
                _,
                _,
            ) => {
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
}
