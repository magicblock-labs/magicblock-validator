use std::ops::ControlFlow;

use mbv_core::traits::ActionError;
use solana_signer::Signer;
use tracing::error;

use crate::{
    execution::{
        error::{IntentExecutorError, IntentExecutorResult},
        transaction::{
            error::TransactionExecutionError,
            utils::{handle_actions_result, handle_commit_id_error, handle_undelegation_error},
        },
    },
    shared::tasks::Task,
};

impl super::runner::TransactionExecutor<'_> {
    /// Repairs rejected work; preparation records already own its cleanup resources.
    /// Follow-ups cannot repair commits, and standalone actions cannot be replayed.
    pub(in crate::execution) async fn recover(
        &mut self,
        err: &TransactionExecutionError,
    ) -> IntentExecutorResult<ControlFlow<()>> {
        let can_repair_commits = self.report.progress.confirmed_commit.is_none()
            && !self.committed_pubkeys.is_empty()
            && self.current.tasks.iter().any(|task| matches!(task, Task::CommitFinalize(_)));
        if !can_repair_commits && self.report.progress.confirmed_commit.is_none() {
            return Ok(ControlFlow::Break(()));
        }

        match err {
            TransactionExecutionError::ActionsError(err, signature) => handle_actions_result(
                &self.ctx.authority.pubkey(),
                self.ctx.actions_callback_executor.as_ref(),
                self.report,
                &mut self.current,
                *signature,
                Err(ActionError::ActionsError(err.clone(), *signature)),
            ),
            TransactionExecutionError::CommitIDError(_, _) if can_repair_commits => {
                handle_commit_id_error(
                    &self.ctx.authority.pubkey(),
                    &self.ctx.task_info_provider,
                    self.committed_pubkeys,
                    &mut self.current,
                    self.intent_id,
                )
                .await?
            }
            TransactionExecutionError::UnfinalizedAccountError(_, _) if can_repair_commits => {
                let task_index = err.task_index(self.current.task_instruction_offset());
                let Some(Task::CommitFinalize(task)) =
                    task_index.and_then(|index| self.current.tasks.get(index as usize))
                else {
                    error!(?task_index, error = ?err, "RPC returned unexpected commit task index");
                    return Ok(ControlFlow::Break(()));
                };
                // Keep the pre-upgrade DLP bridge until #1801's rollout is complete.
                self.ctx
                    .intent_client
                    .finalize_legacy_commit(&self.ctx.authority, task.committed_account.pubkey)
                    .await
                    .map_err(IntentExecutorError::FailedPreparation)?
                    .map_err(IntentExecutorError::FailedExecution)?;
            }
            TransactionExecutionError::UndelegationError(_, _)
                if self.current.tasks.iter().any(|task| matches!(task, Task::Undelegate(_))) =>
            {
                // Report only discarded actions; retained actions still need to execute.
                let callbacks =
                    handle_undelegation_error(&self.ctx.authority.pubkey(), &mut self.current);
                if !callbacks.is_empty() {
                    self.report.add_callback_report(
                        self.ctx.actions_callback_executor.as_ref().schedule(
                            callbacks,
                            err.signature(),
                            Err(ActionError::from(err)),
                        ),
                    );
                }
            }
            _ => return Ok(ControlFlow::Break(())),
        };
        Ok(ControlFlow::Continue(()))
    }
}
