pub(crate) mod cleanup;
pub(crate) mod client;
pub(crate) mod error;
pub(crate) mod intent;
pub(crate) mod progress;
pub(crate) mod transaction;
pub(crate) mod utils;

#[cfg(test)]
mod test_utils;

use std::sync::Arc;

use mbv_core::traits::CallbackScheduleError;
use metrics::metrics;
use progress::{ExecutionProgress, TransactionProgress};
use solana_keypair::Keypair;
use solana_signature::Signature;
use transaction::error::TransactionExecutionError;

use crate::{
    execution::{
        client::IntentExecutionClient,
        error::{IntentExecutorError, IntentExecutorResult},
        preparation::cleanup_resources::CleanupResources,
    },
    shared::account_state::CachedTaskInfoProvider,
};

pub(crate) struct ExecutionContext {
    pub authority: Keypair,
    pub intent_client: IntentExecutionClient,
    pub transaction_preparator: Arc<dyn preparation::TransactionPreparator>,
    pub task_info_provider: Arc<CachedTaskInfoProvider>,
    pub outbox_client: Arc<dyn crate::shared::outbox::OutboxClient>,
    pub actions_callback_executor: Arc<dyn mbv_core::traits::ActionsCallbackScheduler>,
    pub actions_timeout: std::time::Duration,
    pub maintenance: tokio_util::task::TaskTracker,
    pub cancel: tokio_util::sync::CancellationToken,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct ExecutionOutput {
    pub confirmed_commit: Option<Signature>,
    pub signature: Signature,
}

impl ExecutionOutput {
    pub(crate) fn progress(self) -> ExecutionProgress {
        ExecutionProgress {
            confirmed_commit: self.confirmed_commit,
            transaction: TransactionProgress::Confirmed(self.signature),
        }
    }
}

impl metrics::LabelValue for ExecutionOutput {
    fn value(&self) -> &str {
        // Keep existing dashboard labels while the internal stage types disappear.
        if self.confirmed_commit.is_some() {
            "two_stage_succeeded"
        } else {
            "single_stage_succeeded"
        }
    }
}

pub(crate) struct IntentExecutionResult {
    /// Final result of Intent Execution
    pub inner: IntentExecutorResult<ExecutionOutput>,
    pub progress: ExecutionProgress,
    pub callbacks_reported: bool,
}

impl IntentExecutionResult {
    /// Whether a failed execution is safe to retry with a fresh attempt.
    ///
    /// Never retries once action callbacks have reported (a retry would
    /// double-report the outcome), and only retries plausibly transient
    /// errors. Commit tasks give on-chain dedup (commit nonce) to re-executed
    /// sends; action-only intents (`has_dedup_guard == false`) have no such
    /// guard and can double-execute if their transaction landed unobserved,
    /// so they only retry pre-send failures.
    pub(crate) fn is_retriable(&self, has_dedup_guard: bool) -> bool {
        let Err(err) = &self.inner else { return false };
        if self.callbacks_reported || !self.progress.can_retry() || !err.is_transient() {
            return false;
        }
        has_dedup_guard || !matches!(err, IntentExecutorError::FailedExecution(_))
    }

    pub(crate) fn is_ok(&self) -> bool {
        self.inner.is_ok()
    }
}

#[derive(Default)]
pub(crate) struct IntentExecutionReport {
    pub progress: ExecutionProgress,
    /// Resource ownership from each preparation attempt, including failed attempts.
    cleanup: Vec<CleanupResources>,
    /// Errors we patched trying to recover intent
    patched_errors: Vec<TransactionExecutionError>,
    /// Report of scheduled callbacks
    callbacks_report: Vec<Result<Signature, CallbackScheduleError>>,
}

impl IntentExecutionReport {
    pub(crate) fn add_patched_error(&mut self, value: TransactionExecutionError) {
        self.patched_errors.push(value);
    }

    pub(crate) fn patched_errors(&self) -> &[TransactionExecutionError] {
        &self.patched_errors
    }

    pub(crate) fn callbacks_report(&self) -> &[Result<Signature, CallbackScheduleError>] {
        &self.callbacks_report
    }

    pub(crate) fn add_callback_report(
        &mut self,
        values: impl IntoIterator<Item = Result<Signature, CallbackScheduleError>>,
    ) {
        self.callbacks_report.extend(values);
    }
}

#[async_trait::async_trait]
pub(crate) trait IntentRunner: Send + Sync {
    async fn execute(
        self: Arc<Self>,
        intent: &program::outbox_intent_bundles::OutboxIntentBundle,
    ) -> IntentExecutionResult;
    async fn reconcile_intent(
        &self,
        intent: &program::outbox_intent_bundles::OutboxIntentBundle,
    ) -> program::outbox_intent_bundles::OutboxIntentBundle {
        intent.clone()
    }
}

#[async_trait::async_trait]
impl IntentRunner for ExecutionContext {
    async fn execute(
        self: Arc<Self>,
        intent: &program::outbox_intent_bundles::OutboxIntentBundle,
    ) -> IntentExecutionResult {
        let (result, resources) = self.execute_intent(&intent.inner, intent.status()).await;
        let close_buffers = result.is_ok();
        let output = result.inner.as_ref().ok().copied();
        let ctx = self.clone();
        self.maintenance.spawn(async move {
            tokio::select! {
                biased;
                _ = ctx.cancel.cancelled() => {},
                _ = async {
                    let cleanup = async {
                        if let Err(err) = cleanup::clean(&ctx.authority, ctx.transaction_preparator.as_ref(), resources, close_buffers).await {
                            tracing::error!(error = ?err, "Failed to clean up intent resources");
                        }
                    };
                    let metrics = async { if let Some(output) = output { ctx.intent_client.intent_metrics(output).await } };
                    tokio::join!(cleanup, metrics);
                } => {},
            }
        });
        result
    }

    async fn reconcile_intent(
        &self,
        intent: &program::outbox_intent_bundles::OutboxIntentBundle,
    ) -> program::outbox_intent_bundles::OutboxIntentBundle {
        match self.outbox_client.fetch_outbox_intent(intent.intent_id).await {
            Ok(Some(bundle)) => bundle,
            Ok(None) => intent.clone(),
            Err(err) => {
                tracing::warn!(intent_id = intent.intent_id, error = ?err, "Failed to reconcile outbox intent before retry");
                intent.clone()
            }
        }
    }
}

pub(crate) mod backlog;
pub(crate) mod preparation;
pub(crate) mod queue;
pub(crate) mod report;
pub(crate) mod scheduler;
pub(crate) mod stream;
pub(crate) mod task_builder;
pub(crate) mod worker;
pub(crate) use worker::IntentExecutor;
