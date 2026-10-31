use std::time::{Duration, Instant};

use async_trait::async_trait;
use mbv_core::traits::ActionsCallbackScheduler;
use program::{
    magic_scheduled_base_intent::ScheduledIntentBundle,
    outbox::{ExecutionStage, TwoStageProgress},
    outbox_intent_bundles::OutboxIntentBundleStatus,
};
use solana_pubkey::Pubkey;
use solana_signature::Signature;
use solana_signer::Signer;

use crate::{
    intent_executor::{
        ExecutionOutput, IntentExecutionReport, IntentExecutionResult, IntentExecutor,
        IntentExecutorCtx,
        cleanup_handle::CleanupHandle,
        error::{IntentExecutorError, IntentExecutorResult},
        strategy_executor::{
            transaction::TransactionExecutor,
            utils::{requires_uniqueness_nonce, resolve_pending_signature},
        },
        utils::{build_commit_finalize_tasks, report_and_close_intent},
    },
    outbox::{OutboxClient, ScheduledBaseIntentMeta},
    tasks::{
        task_builder::{TaskBuilderError, TaskBuilderImpl, TasksBuilder},
        task_info_fetcher::{AccountSnapshot, ResetType, TaskInfoFetcher},
        task_strategist::{StrategyExecutionMode, TaskStrategist, TransactionStrategy},
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
        let confirmed_commit = match &self.status {
            OutboxIntentBundleStatus::Accepted => {
                if intent.is_empty() {
                    return Err(IntentExecutorError::EmptyIntentError);
                }
                None
            }
            OutboxIntentBundleStatus::Executing(stage) => {
                // Resolve the old transaction before building replacement work: it may still land.
                let succeeded =
                    resolve_pending_signature(&self.ctx.intent_client, stage.pending_transaction())
                        .await?;
                match (stage, succeeded) {
                    (ExecutionStage::SingleStage(pending), true) => {
                        return Ok(ExecutionOutput::SingleStage(pending.signature));
                    }
                    (
                        ExecutionStage::TwoStage(TwoStageProgress::Finalizing { commit, finalize }),
                        true,
                    ) => {
                        return Ok(ExecutionOutput::TwoStage {
                            commit_signature: *commit,
                            finalize_signature: finalize.signature,
                        });
                    }
                    (ExecutionStage::TwoStage(TwoStageProgress::Committing(pending)), true) => {
                        Some(pending.signature)
                    }
                    (
                        ExecutionStage::TwoStage(TwoStageProgress::Finalizing { commit, .. }),
                        false,
                    ) => Some(*commit),
                    _ => None,
                }
            }
        };

        let committed_pubkeys = intent.get_all_committed_pubkeys();
        let executor = if let Some(commit_signature) = confirmed_commit {
            let strategy = self.build_follow_up(intent, commit_signature).await?;
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
    ) -> IntentExecutorResult<StrategyExecutionMode> {
        let authority = self.ctx.authority.pubkey();
        if matches!(self.status, OutboxIntentBundleStatus::Accepted) && committed_pubkeys.is_empty()
        {
            // Standalone actions use one transaction with a per-intent uniqueness nonce.
            let tasks = TaskBuilderImpl::commit_tasks(&self.ctx.task_info_fetcher, intent).await?;
            return Ok(StrategyExecutionMode::SingleStage(
                TaskStrategist::build_strategy(tasks, &authority, Some(intent.intent_id))?,
            ));
        }

        let (mut commit_tasks, follow_up_tasks) =
            build_commit_finalize_tasks(intent, &self.ctx.task_info_fetcher).await?;
        // Resumed standalone actions still need their per-intent uniqueness nonce.
        let nonce = (committed_pubkeys.is_empty() || requires_uniqueness_nonce(&commit_tasks))
            .then_some(intent.intent_id);
        // Resumed work must respect the mode already recorded in the outbox.
        match &self.status {
            OutboxIntentBundleStatus::Executing(ExecutionStage::SingleStage(_)) => {
                commit_tasks.extend(follow_up_tasks);
                Ok(StrategyExecutionMode::SingleStage(
                    TaskStrategist::build_strategy(commit_tasks, &authority, nonce)?,
                ))
            }
            status => Ok(match status {
                OutboxIntentBundleStatus::Accepted => TaskStrategist::build_execution_strategy(
                    commit_tasks,
                    follow_up_tasks,
                    &authority,
                    nonce,
                )?,
                _ => TaskStrategist::build_two_stage(
                    commit_tasks,
                    follow_up_tasks,
                    &authority,
                    nonce,
                )?
                .into(),
            }),
        }
    }

    async fn build_follow_up(
        &self,
        intent: &ScheduledIntentBundle,
        commit_signature: Signature,
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
                .map_err(TaskBuilderError::FinalizedTasksBuildError)?;
            let pubkeys: Vec<_> = accounts.iter().map(|(pubkey, _)| *pubkey).collect();
            self.ctx
                .intent_client
                .finalize_legacy_commits(&self.ctx.authority, &pubkeys, commit_signature)
                .await?;
        }

        // The commit is confirmed; only actions and undelegation remain.
        let tasks = TaskBuilderImpl::finalize_tasks(&self.ctx.task_info_fetcher, intent).await?;
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
            self.ctx.task_info_fetcher.reset(ResetType::Specific(&pubkeys));
        } else if !undelegated_pubkeys.is_empty() {
            // Keep other nonces cached: a chain re-fetch can race the just-landed
            // commit and reuse a nonce (buffer PDA collision).
            self.ctx.task_info_fetcher.reset(ResetType::Specific(&undelegated_pubkeys));
        }
        if let Ok(output) = &result {
            let output = *output;
            let intent_client = self.ctx.intent_client.clone();
            tokio::spawn(async move { intent_client.intent_metrics(output).await });
        }

        let cleanup_handle = CleanupHandle::new(
            self.ctx.authority,
            report.junk,
            result.is_ok(),
            self.ctx.transaction_preparator,
        );
        let result = IntentExecutionResult {
            inner: result,
            patched_errors: report.patched_errors,
            callbacks_report: report.callbacks_report,
        };
        (result, cleanup_handle)
    }
}
