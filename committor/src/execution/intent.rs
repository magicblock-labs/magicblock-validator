#[cfg(test)]
use std::time::Duration;
use std::time::Instant;

use program::{
    magic_scheduled_base_intent::ScheduledIntentBundle,
    outbox_intent_bundles::OutboxIntentBundleStatus,
};
use solana_pubkey::Pubkey;
use solana_signer::Signer;

use crate::{
    execution::{
        ExecutionContext, ExecutionOutput, IntentExecutionReport, IntentExecutionResult,
        error::{IntentExecutorError, IntentExecutorResult},
        progress::{ExecutionProgress, TransactionProgress},
        task_builder::{TaskBuilder, TaskBuilderError},
        transaction::{
            runner::TransactionExecutor,
            utils::{requires_uniqueness_nonce, resolve_pending_signature},
        },
        utils::report_and_close_intent,
    },
    shared::{
        account_state::{AccountSnapshot, TaskInfoProvider},
        outbox::{ScheduledBaseIntentMeta, progress::PendingExecution},
        tasks::task_strategist::{TaskStrategist, TransactionPlan, TransactionStrategy},
    },
};

#[cfg(test)]
mod tests;

impl ExecutionContext {
    async fn execute_inner(
        &self,
        status: &OutboxIntentBundleStatus,
        intent: &ScheduledIntentBundle,
        report: &mut IntentExecutionReport,
        started_at: Instant,
    ) -> IntentExecutorResult<ExecutionOutput> {
        if let Some(pending) = PendingExecution::from_status(status) {
            report.progress = ExecutionProgress {
                confirmed_commit: pending.confirmed_commit,
                transaction: TransactionProgress::Submitted(pending.transaction.signature),
            };
            // Resolve the old transaction before building replacement work: it may still land.
            let succeeded =
                resolve_pending_signature(&self.intent_client, &pending.transaction).await?;
            if succeeded {
                report.progress.transaction =
                    TransactionProgress::Confirmed(pending.transaction.signature);
                if !pending.has_follow_up {
                    return Ok(ExecutionOutput {
                        confirmed_commit: pending.confirmed_commit,
                        signature: pending.transaction.signature,
                    });
                }
                report.progress = ExecutionProgress {
                    confirmed_commit: Some(pending.transaction.signature),
                    ..Default::default()
                };
            } else {
                report.progress.transaction =
                    TransactionProgress::Rejected(pending.transaction.signature);
            }
        } else if intent.is_empty() {
            return Err(IntentExecutorError::EmptyIntentError);
        }

        let committed_pubkeys = intent.get_all_committed_pubkeys();
        let executor = if let Some(commit_signature) = report.progress.confirmed_commit {
            let strategy = self.build_follow_up(intent).await?;
            TransactionExecutor::resume_follow_up(
                self,
                report,
                intent.intent_id,
                commit_signature,
                strategy,
            )
        } else {
            let strategy = self.build_strategy(status, intent, &committed_pubkeys).await?;
            TransactionExecutor::new(self, report, intent.intent_id, &committed_pubkeys, strategy)
        };
        executor
            .execute(|| self.actions_timeout.checked_sub(started_at.elapsed()))
            .await
    }

    async fn build_strategy(
        &self,
        status: &OutboxIntentBundleStatus,
        intent: &ScheduledIntentBundle,
        committed_pubkeys: &[Pubkey],
    ) -> IntentExecutorResult<TransactionPlan> {
        let authority = self.authority.pubkey();
        let pending = PendingExecution::from_status(status);
        if pending.is_none() && committed_pubkeys.is_empty() {
            // Standalone actions use one transaction with a per-intent uniqueness nonce.
            let tasks = TaskBuilder::commit_tasks(&self.task_info_provider, intent).await?;
            return Ok(
                TaskStrategist::build_strategy(tasks, &authority, Some(intent.intent_id))?.into(),
            );
        }

        let (commit_tasks, follow_up_tasks) = futures_util::future::join(
            TaskBuilder::commit_tasks(&self.task_info_provider, intent),
            TaskBuilder::follow_up_tasks(&self.task_info_provider, intent),
        )
        .await;
        let (mut commit_tasks, follow_up_tasks) = (commit_tasks?, follow_up_tasks?);
        // Resumed standalone actions still need their per-intent uniqueness nonce.
        let nonce = (committed_pubkeys.is_empty() || requires_uniqueness_nonce(&commit_tasks))
            .then_some(intent.intent_id);
        // Resumed work must respect the plan already recorded in the outbox.
        if pending.is_some_and(|progress| !progress.has_follow_up) {
            commit_tasks.extend(follow_up_tasks);
            return Ok(TaskStrategist::build_strategy(commit_tasks, &authority, nonce)?.into());
        }
        Ok(if pending.is_some() {
            TaskStrategist::build_split_plan(commit_tasks, follow_up_tasks, &authority, nonce)?
        } else {
            TaskStrategist::build_plan(commit_tasks, follow_up_tasks, &authority, nonce)?
        })
    }

    async fn build_follow_up(
        &self,
        intent: &ScheduledIntentBundle,
    ) -> IntentExecutorResult<TransactionStrategy> {
        let accounts: Vec<AccountSnapshot> = intent
            .get_commit_intent_accounts()
            .into_iter()
            .chain(intent.get_undelegate_intent_accounts())
            .flatten()
            .map(|account| (account.pubkey, account.remote_slot))
            .collect();
        if !accounts.is_empty() {
            let min_context_slot = accounts.iter().map(|(_, slot)| *slot).max().unwrap_or_default();
            self.task_info_provider
                .fetch_delegation_metadata(&accounts, min_context_slot)
                .await
                .map_err(TaskBuilderError::FollowUpTasksBuildError)?;
            let pubkeys: Vec<_> = accounts.iter().map(|(pubkey, _)| *pubkey).collect();
            self.intent_client.finalize_legacy_commits(&self.authority, &pubkeys).await?;
        }

        // The commit is confirmed; only actions and undelegation remain.
        let tasks = TaskBuilder::follow_up_tasks(&self.task_info_provider, intent).await?;
        let uniqueness_nonce = self.follow_up_uniqueness_nonce(intent).await?;
        Ok(TaskStrategist::build_strategy(
            tasks,
            &self.authority.pubkey(),
            uniqueness_nonce,
        )?)
    }

    async fn follow_up_uniqueness_nonce(
        &self,
        intent: &ScheduledIntentBundle,
    ) -> IntentExecutorResult<Option<u64>> {
        let accounts = intent
            .get_all_committed_accounts()
            .into_iter()
            .map(|account| (account.pubkey, account.remote_slot))
            .collect::<Vec<AccountSnapshot>>();
        if accounts.is_empty() {
            return Ok(None);
        }
        let min_context_slot = accounts.iter().map(|(_, slot)| *slot).max().unwrap_or_default();
        let nonces = self
            .task_info_provider
            .fetch_current_commit_nonces(&accounts, min_context_slot)
            .await
            .map_err(TaskBuilderError::CommitTasksBuildError)?;
        Ok(nonces.values().any(|nonce| *nonce <= 1).then_some(intent.intent_id))
    }
}

impl ExecutionContext {
    pub(crate) async fn execute_intent(
        &self,
        intent: &ScheduledIntentBundle,
        status: &OutboxIntentBundleStatus,
    ) -> (
        IntentExecutionResult,
        Vec<crate::execution::preparation::cleanup_resources::CleanupResources>,
    ) {
        let started_at = Instant::now();
        let meta = ScheduledBaseIntentMeta::new(intent);
        let pubkeys = intent.get_all_committed_pubkeys();
        let undelegated_pubkeys = intent.get_undelegated_pubkeys();
        let mut report = IntentExecutionReport::default();
        let result = self.execute_inner(status, intent, &mut report, started_at).await;
        let result =
            report_and_close_intent(result, meta, &mut report, self.outbox_client.as_ref()).await;

        if result.is_err() {
            // We can't know what landed on chain, resync everything.
            self.task_info_provider.invalidate_nonces(&pubkeys);
        } else if !undelegated_pubkeys.is_empty() {
            // Keep other nonces cached: a chain re-fetch can race the just-landed
            // commit and reuse a nonce (buffer PDA collision).
            self.task_info_provider.invalidate_nonces(&undelegated_pubkeys);
        }
        let result = IntentExecutionResult {
            inner: result,
            progress: report.progress,
            callbacks_reported: !report.callbacks_report.is_empty(),
        };
        (result, report.cleanup)
    }
}
