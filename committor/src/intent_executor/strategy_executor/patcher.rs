use std::ops::ControlFlow;

use async_trait::async_trait;
use mbv_core::traits::{ActionError, ActionsCallbackScheduler};
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use tracing::{error, warn};

use crate::{
    intent_executor::{
        IntentExecutionReport,
        error::{IntentExecutorError, IntentExecutorResult},
        intent_execution_client::IntentExecutionClient,
        strategy_executor::{
            error::TransactionStrategyExecutionError,
            utils::{handle_actions_result, handle_commit_id_error, handle_undelegation_error},
        },
    },
    tasks::{
        BaseTaskImpl,
        task_info_fetcher::{CacheTaskInfoFetcher, TaskInfoFetcher},
        task_strategist::TransactionStrategy,
    },
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

pub(in crate::intent_executor) struct SingleStagePatcher<'a, F, A> {
    pub authority: &'a Keypair,
    pub intent_id: u64,
    pub intent_client: &'a IntentExecutionClient,
    pub callback_scheduler: &'a A,
    pub task_info_fetcher: &'a CacheTaskInfoFetcher<F>,
    pub committed_pubkeys: &'a [Pubkey],
}

impl<F, A> SingleStagePatcher<'_, F, A> {
    /// Handles unfinalized account error for single stage execution.
    /// Sends a separate tx to finalize account and then continues execution.
    async fn handle_unfinalized_account_error(
        &self,
        delegated_account: Pubkey,
    ) -> IntentExecutorResult<ControlFlow<(), TransactionStrategy>> {
        self.intent_client
            .finalize_account(self.authority, delegated_account)
            .await
            .map_err(IntentExecutorError::FailedPreparation)?
            .map_err(IntentExecutorError::FailedExecution)?;
        Ok(ControlFlow::Continue(TransactionStrategy::default()))
    }
}

#[async_trait]
impl<F, A> Patcher for SingleStagePatcher<'_, F, A>
where
    F: TaskInfoFetcher,
    A: ActionsCallbackScheduler,
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
                    self.intent_id,
                )
                .await?;
                Ok(ControlFlow::Continue(to_cleanup))
            }
            err @ TransactionStrategyExecutionError::UnfinalizedAccountError(_, _) => {
                let optimized_tasks = strategy.optimized_tasks.as_slice();
                if let Some(delegated_account) = err
                    .task_index(strategy.task_instruction_offset())
                    .and_then(|index| optimized_tasks.get(index as usize))
                    .and_then(|task| match task {
                        BaseTaskImpl::CommitFinalize(task) => Some(task.committed_account.pubkey),
                        _ => None,
                    })
                {
                    self.handle_unfinalized_account_error(delegated_account).await
                } else {
                    error!(
                        task_index = err.task_index(strategy.task_instruction_offset()),
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
                let to_cleanup = handle_undelegation_error(&self.authority.pubkey(), strategy);
                Ok(ControlFlow::Continue(to_cleanup))
            }
            TransactionStrategyExecutionError::CpiLimitError(_, _)
            | TransactionStrategyExecutionError::LoadedAccountsDataSizeExceeded(_, _)
            | TransactionStrategyExecutionError::TransactionTooLargeError(_) => {
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

pub(in crate::intent_executor) struct CommitStagePatcher<'a, F, A> {
    pub authority: &'a Keypair,
    pub intent_id: u64,
    pub intent_client: &'a IntentExecutionClient,
    pub callback_scheduler: &'a A,
    pub task_info_fetcher: &'a CacheTaskInfoFetcher<F>,
    pub committed_pubkeys: &'a [Pubkey],
}

impl<F, A> CommitStagePatcher<'_, F, A> {
    /// Handles unfinalized account error for commit stage execution.
    /// Sends a separate tx to finalize account and then continues execution.
    async fn handle_unfinalized_account_error(
        &self,
        delegated_account: Pubkey,
    ) -> IntentExecutorResult<ControlFlow<(), TransactionStrategy>> {
        self.intent_client
            .finalize_account(self.authority, delegated_account)
            .await
            .map_err(IntentExecutorError::FailedPreparation)?
            .map_err(IntentExecutorError::FailedExecution)?;
        Ok(ControlFlow::Continue(TransactionStrategy::default()))
    }
}

#[async_trait]
impl<F, A> Patcher for CommitStagePatcher<'_, F, A>
where
    F: TaskInfoFetcher,
    A: ActionsCallbackScheduler,
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
                    self.intent_id,
                )
                .await?;
                Ok(ControlFlow::Continue(to_cleanup))
            }
            err @ TransactionStrategyExecutionError::UnfinalizedAccountError(_, _) => {
                let optimized_tasks = strategy.optimized_tasks.as_slice();
                let task_index = err.task_index(strategy.task_instruction_offset());
                if let Some(delegated_account) = task_index
                    .and_then(|index| optimized_tasks.get(index as usize))
                    .and_then(|task| match task {
                        BaseTaskImpl::CommitFinalize(task) => Some(task.committed_account.pubkey),
                        _ => None,
                    })
                {
                    self.handle_unfinalized_account_error(delegated_account).await
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
            | TransactionStrategyExecutionError::LoadedAccountsDataSizeExceeded(_, _) => {
                // Can't be handled
                error!(error = ?err, "Commit tasks exceeded execution limit");
                Ok(ControlFlow::Break(()))
            }
            TransactionStrategyExecutionError::TransactionTooLargeError(err) => {
                error!(error = ?err, "Failed to execute intent: transaction too large!");
                Ok(ControlFlow::Break(()))
            }
            TransactionStrategyExecutionError::InternalError(_) => {
                // Can't be handled
                Ok(ControlFlow::Break(()))
            }
        }
    }
}

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
                let to_cleanup = handle_undelegation_error(&self.authority.pubkey(), strategy);
                Ok(ControlFlow::Continue(to_cleanup))
            }
            TransactionStrategyExecutionError::CpiLimitError(_, _)
            | TransactionStrategyExecutionError::LoadedAccountsDataSizeExceeded(_, _) => {
                // Can't be handled
                warn!(error = ?err, "Finalization tasks exceeded execution limit");
                Ok(ControlFlow::Break(()))
            }
            TransactionStrategyExecutionError::TransactionTooLargeError(err) => {
                error!(error = ?err, "Failed to execute intent: transaction too large!");
                Ok(ControlFlow::Break(()))
            }
            TransactionStrategyExecutionError::InternalError(_) => {
                // Can't be handled
                Ok(ControlFlow::Break(()))
            }
        }
    }
}
