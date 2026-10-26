use std::time::{Duration, Instant};

use async_trait::async_trait;
use mbv_core::traits::ActionsCallbackScheduler;
use program::{
    magic_scheduled_base_intent::ScheduledIntentBundle,
    outbox_intent_bundles::OutboxIntentBundleStatus,
};
use solana_pubkey::Pubkey;
use solana_signer::Signer;

use crate::{
    intent_executor::{
        ExecutionOutput, IntentExecutionReport, IntentExecutionResult, IntentExecutor,
        IntentExecutorCtx,
        cleanup_handle::CleanupHandle,
        error::{IntentExecutorError, IntentExecutorResult},
        progress::{ExecutionProgress, TransactionProgress},
        strategy_executor::{
            transaction::TransactionExecutor,
            utils::{requires_uniqueness_nonce, resolve_pending_signature},
        },
        utils::{build_intent_tasks, report_and_close_intent},
    },
    outbox::{OutboxClient, ScheduledBaseIntentMeta, progress::PendingExecution},
    tasks::{
        task_builder::{TaskBuilder, TaskBuilderError},
        task_info_fetcher::{AccountSnapshot, TaskInfoFetcher},
        task_strategist::{TaskStrategist, TransactionPlan, TransactionStrategy},
    },
    transaction_preparator::TransactionPreparator,
};

#[cfg(test)]
mod tests;

/// Executes fresh or resumed intents from their recorded outbox progress.
pub(crate) struct IntentExecutorImpl<T, F, A, O> {
    ctx: IntentExecutorCtx<T, F, A, O>,
    status: OutboxIntentBundleStatus,
    actions_timeout: Duration,
}

impl<T, F, A, O> IntentExecutorImpl<T, F, A, O>
where
    T: TransactionPreparator,
    F: TaskInfoFetcher,
    A: ActionsCallbackScheduler,
    O: OutboxClient,
    O::Error: Into<IntentExecutorError>,
{
    pub(crate) fn new(
        ctx: IntentExecutorCtx<T, F, A, O>,
        status: OutboxIntentBundleStatus,
        actions_timeout: Duration,
    ) -> Self {
        Self { ctx, status, actions_timeout }
    }

    async fn execute_inner(
        &self,
        intent: &ScheduledIntentBundle,
        report: &mut IntentExecutionReport,
        started_at: Instant,
    ) -> IntentExecutorResult<ExecutionOutput> {
        if let Some(pending) = PendingExecution::from_status(&self.status) {
            report.progress = ExecutionProgress {
                confirmed_commit: pending.confirmed_commit,
                transaction: TransactionProgress::Submitted(pending.transaction.signature),
            };
            // Resolve the old transaction before building replacement work: it may still land.
            let succeeded =
                resolve_pending_signature(&self.ctx.intent_client, &pending.transaction).await?;
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
                &self.ctx,
                report,
                intent.intent_id,
                commit_signature,
                strategy,
            )
        } else {
            let strategy = self.build_strategy(intent, &committed_pubkeys).await?;
            TransactionExecutor::new(
                &self.ctx,
                report,
                intent.intent_id,
                &committed_pubkeys,
                strategy,
            )
        };
        executor
            .execute(|| self.actions_timeout.checked_sub(started_at.elapsed()))
            .await
    }

    async fn build_strategy(
        &self,
        intent: &ScheduledIntentBundle,
        committed_pubkeys: &[Pubkey],
    ) -> IntentExecutorResult<TransactionPlan> {
        let authority = self.ctx.authority.pubkey();
        let pending = PendingExecution::from_status(&self.status);
        if pending.is_none() && committed_pubkeys.is_empty() {
            // Standalone actions use one transaction with a per-intent uniqueness nonce.
            let tasks = TaskBuilder::commit_tasks(&self.ctx.task_info_fetcher, intent).await?;
            return Ok(
                TaskStrategist::build_strategy(tasks, &authority, Some(intent.intent_id))?.into(),
            );
        }

        let (mut commit_tasks, follow_up_tasks) =
            build_intent_tasks(intent, &self.ctx.task_info_fetcher).await?;
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
            self.ctx
                .task_info_fetcher
                .fetch_delegation_metadata(&accounts, min_context_slot)
                .await
                .map_err(TaskBuilderError::FollowUpTasksBuildError)?;
            let pubkeys: Vec<_> = accounts.iter().map(|(pubkey, _)| *pubkey).collect();
            self.ctx
                .intent_client
                .finalize_legacy_commits(&self.ctx.authority, &pubkeys)
                .await?;
        }

        // The commit is confirmed; only actions and undelegation remain.
        let tasks = TaskBuilder::follow_up_tasks(&self.ctx.task_info_fetcher, intent).await?;
        let uniqueness_nonce = self.follow_up_uniqueness_nonce(intent).await?;
        Ok(TaskStrategist::build_strategy(
            tasks,
            &self.ctx.authority.pubkey(),
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
            .ctx
            .task_info_fetcher
            .fetch_current_commit_nonces(&accounts, min_context_slot)
            .await
            .map_err(TaskBuilderError::CommitTasksBuildError)?;
        Ok(nonces.values().any(|nonce| *nonce <= 1).then_some(intent.intent_id))
    }
}

#[async_trait]
impl<T, F, A, O> IntentExecutor<T> for IntentExecutorImpl<T, F, A, O>
where
    T: TransactionPreparator,
    F: TaskInfoFetcher,
    A: ActionsCallbackScheduler,
    O: OutboxClient,
    O::Error: Into<IntentExecutorError>,
{
    async fn execute(
        self: Box<Self>,
        intent: ScheduledIntentBundle,
    ) -> (IntentExecutionResult, CleanupHandle<T>) {
        let started_at = Instant::now();
        let meta = ScheduledBaseIntentMeta::new(&intent);
        let pubkeys = intent.get_all_committed_pubkeys();
        let undelegated_pubkeys = intent.get_undelegated_pubkeys();
        let mut report = IntentExecutionReport::default();
        let result = self.execute_inner(&intent, &mut report, started_at).await;
        let result =
            report_and_close_intent(result, meta, &mut report, self.ctx.outbox_client.as_ref())
                .await;

        if result.is_err() {
            // We can't know what landed on chain, resync everything.
            self.ctx.task_info_fetcher.invalidate_nonces(&pubkeys);
        } else if !undelegated_pubkeys.is_empty() {
            // Keep other nonces cached: a chain re-fetch can race the just-landed
            // commit and reuse a nonce (buffer PDA collision).
            self.ctx.task_info_fetcher.invalidate_nonces(&undelegated_pubkeys);
        }
        if let Ok(output) = &result {
            let output = *output;
            let intent_client = self.ctx.intent_client.clone();
            tokio::spawn(async move { intent_client.intent_metrics(output).await });
        }

        let cleanup_handle = CleanupHandle::new(
            self.ctx.authority,
            report.cleanup,
            result.is_ok(),
            self.ctx.transaction_preparator,
        );
        let result = IntentExecutionResult {
            inner: result,
            progress: report.progress,
            patched_errors: report.patched_errors,
            callbacks_report: report.callbacks_report,
        };
        (result, cleanup_handle)
    }
}
