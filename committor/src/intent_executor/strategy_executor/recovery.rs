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
            error::TransactionExecutionError,
            utils::{handle_actions_result, handle_commit_id_error, handle_undelegation_error},
        },
    },
    tasks::{
        Task,
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
    /// Repairs rejected work; preparation records already own its cleanup resources.
    /// Follow-ups cannot repair commits, and standalone actions cannot be replayed.
    pub(in crate::intent_executor) async fn recover(
        &self,
        err: &TransactionExecutionError,
        strategy: &mut TransactionStrategy,
        report: &mut IntentExecutionReport,
    ) -> IntentExecutorResult<ControlFlow<()>> {
        let can_repair_commits = report.progress.confirmed_commit.is_none()
            && !self.committed_pubkeys.is_empty()
            && strategy.tasks.iter().any(|task| matches!(task, Task::CommitFinalize(_)));
        if !can_repair_commits && report.progress.confirmed_commit.is_none() {
            return Ok(ControlFlow::Break(()));
        }

        match err {
            TransactionExecutionError::ActionsError(err, signature) => handle_actions_result(
                &self.authority.pubkey(),
                self.callback_scheduler,
                report,
                strategy,
                *signature,
                Err(ActionError::ActionsError(err.clone(), *signature)),
            ),
            TransactionExecutionError::CommitIDError(_, _) if can_repair_commits => {
                handle_commit_id_error(
                    &self.authority.pubkey(),
                    self.task_info_fetcher,
                    self.committed_pubkeys,
                    strategy,
                    self.intent_id,
                )
                .await?
            }
            TransactionExecutionError::UnfinalizedAccountError(_, _) if can_repair_commits => {
                let task_index = err.task_index(strategy.task_instruction_offset());
                let Some(Task::CommitFinalize(task)) =
                    task_index.and_then(|index| strategy.tasks.get(index as usize))
                else {
                    error!(?task_index, error = ?err, "RPC returned unexpected commit task index");
                    return Ok(ControlFlow::Break(()));
                };
                // Keep the pre-upgrade DLP bridge until #1801's rollout is complete.
                self.intent_client
                    .finalize_legacy_commit(self.authority, task.committed_account.pubkey)
                    .await
                    .map_err(IntentExecutorError::FailedPreparation)?
                    .map_err(IntentExecutorError::FailedExecution)?;
            }
            TransactionExecutionError::UndelegationError(_, _)
                if strategy.tasks.iter().any(|task| matches!(task, Task::Undelegate(_))) =>
            {
                // Report only discarded actions; retained actions still need to execute.
                let callbacks = handle_undelegation_error(&self.authority.pubkey(), strategy);
                if !callbacks.is_empty() {
                    report.add_callback_report(self.callback_scheduler.schedule(
                        callbacks,
                        err.signature(),
                        Err(ActionError::from(err)),
                    ));
                }
            }
            _ => return Ok(ControlFlow::Break(())),
        };
        Ok(ControlFlow::Continue(()))
    }
}
