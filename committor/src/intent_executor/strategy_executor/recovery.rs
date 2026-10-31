use std::ops::ControlFlow;

use mbv_core::traits::{ActionError, ActionsCallbackScheduler};
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use tracing::error;

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

pub(in crate::intent_executor) struct TransactionRecovery<'a, F, A> {
    pub authority: &'a Keypair,
    pub intent_id: u64,
    pub intent_client: &'a IntentExecutionClient,
    pub callback_scheduler: &'a A,
    pub task_info_fetcher: &'a CacheTaskInfoFetcher<F>,
    pub committed_pubkeys: &'a [Pubkey],
}

impl<F: TaskInfoFetcher, A: ActionsCallbackScheduler> TransactionRecovery<'_, F, A> {
    /// Repairs rejected work and returns resources to clean up before completion.
    /// Follow-ups cannot repair commits, and standalone actions cannot be replayed.
    pub(in crate::intent_executor) async fn recover(
        &self,
        err: &TransactionStrategyExecutionError,
        strategy: &mut TransactionStrategy,
        report: &mut IntentExecutionReport,
    ) -> IntentExecutorResult<ControlFlow<(), TransactionStrategy>> {
        let can_repair_commits = report.progress.confirmed_commit.is_none()
            && !self.committed_pubkeys.is_empty()
            && strategy
                .optimized_tasks
                .iter()
                .any(|task| matches!(task, BaseTaskImpl::CommitFinalize(_)));
        if !can_repair_commits && report.progress.confirmed_commit.is_none() {
            return Ok(ControlFlow::Break(()));
        }

        let cleanup = match err {
            TransactionStrategyExecutionError::ActionsError(err, signature) => {
                handle_actions_result(
                    &self.authority.pubkey(),
                    self.callback_scheduler,
                    report,
                    strategy,
                    *signature,
                    Err(ActionError::ActionsError(err.clone(), *signature)),
                )
            }
            TransactionStrategyExecutionError::CommitIDError(_, _) if can_repair_commits => {
                handle_commit_id_error(
                    &self.authority.pubkey(),
                    self.task_info_fetcher,
                    self.committed_pubkeys,
                    strategy,
                    self.intent_id,
                )
                .await?
            }
            TransactionStrategyExecutionError::UnfinalizedAccountError(_, _)
                if can_repair_commits =>
            {
                let task_index = err.task_index(strategy.task_instruction_offset());
                let Some(BaseTaskImpl::CommitFinalize(task)) =
                    task_index.and_then(|index| strategy.optimized_tasks.get(index as usize))
                else {
                    error!(?task_index, error = ?err, "RPC returned unexpected commit task index");
                    return Ok(ControlFlow::Break(()));
                };
                // Keep the pre-upgrade DLP bridge until #1801's rollout is complete.
                self.intent_client
                    .finalize_account(self.authority, task.committed_account.pubkey)
                    .await
                    .map_err(IntentExecutorError::FailedPreparation)?
                    .map_err(IntentExecutorError::FailedExecution)?;
                TransactionStrategy::default()
            }
            TransactionStrategyExecutionError::UndelegationError(_, _)
                if strategy
                    .optimized_tasks
                    .iter()
                    .any(|task| matches!(task, BaseTaskImpl::Undelegate(_))) =>
            {
                let mut removed = handle_undelegation_error(&self.authority.pubkey(), strategy);
                // Report only discarded actions; retained actions still need to execute.
                let callbacks = removed.extract_action_callbacks();
                if !callbacks.is_empty() {
                    report.add_callback_report(self.callback_scheduler.schedule(
                        callbacks,
                        err.signature(),
                        Err(ActionError::from(err)),
                    ));
                }
                removed
            }
            _ => return Ok(ControlFlow::Break(())),
        };
        Ok(ControlFlow::Continue(cleanup))
    }
}
