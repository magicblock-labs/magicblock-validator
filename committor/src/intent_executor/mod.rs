pub mod cleanup_handle;
pub mod error;
pub(crate) mod executor;
pub mod intent_execution_client;
pub(crate) mod intent_executor_factory;
pub mod progress;
pub mod strategy_executor;
pub mod utils;

#[cfg(test)]
mod test_utils;

use std::sync::Arc;

use async_trait::async_trait;
use mbv_core::traits::CallbackScheduleError;
use metrics::metrics;
use program::magic_scheduled_base_intent::ScheduledIntentBundle;
use progress::{ExecutionProgress, TransactionProgress};
use solana_keypair::Keypair;
use solana_signature::Signature;
use strategy_executor::error::TransactionExecutionError;

use crate::{
    intent_executor::{
        cleanup_handle::CleanupHandle,
        error::{IntentExecutorError, IntentExecutorResult},
        intent_execution_client::IntentExecutionClient,
    },
    tasks::task_info_fetcher::CacheTaskInfoFetcher,
    transaction_preparator::cleanup_resources::CleanupResources,
};

#[async_trait]
pub trait IntentExecutor<T>: Send + Sync + 'static {
    /// Executes Message on Base layer
    /// Returns result of intent execution `IntentExecutionResult`
    /// and `CleanupHandle` for cleanup after intent
    async fn execute(
        self: Box<Self>,
        base_intent: ScheduledIntentBundle,
    ) -> (IntentExecutionResult, CleanupHandle<T>);
}

pub struct IntentExecutorCtx<T, F, A, O> {
    /// Base-layer signing identity — the engine's authority keypair.
    pub authority: Keypair,
    pub intent_client: IntentExecutionClient,
    pub transaction_preparator: T,
    pub task_info_fetcher: Arc<CacheTaskInfoFetcher<F>>,
    pub outbox_client: Arc<O>,
    pub actions_callback_executor: A,
}

#[derive(Clone, Copy, Debug)]
pub struct ExecutionOutput {
    pub confirmed_commit: Option<Signature>,
    pub signature: Signature,
}

impl ExecutionOutput {
    pub fn progress(self) -> ExecutionProgress {
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

pub struct IntentExecutionResult {
    /// Final result of Intent Execution
    pub inner: IntentExecutorResult<ExecutionOutput>,
    pub progress: ExecutionProgress,
    /// Errors patched along the way
    pub patched_errors: Vec<TransactionExecutionError>,
    /// Callbacks result
    pub callbacks_report: Vec<Result<Signature, CallbackScheduleError>>,
}

impl IntentExecutionResult {
    /// Whether a failed execution is safe to retry with a fresh executor.
    ///
    /// Never retries once action callbacks have reported (a retry would
    /// double-report the outcome), and only retries plausibly transient
    /// errors. Commit tasks give on-chain dedup (commit nonce) to re-executed
    /// sends; action-only intents (`has_dedup_guard == false`) have no such
    /// guard and can double-execute if their transaction landed unobserved,
    /// so they only retry pre-send failures.
    pub fn is_retriable(&self, has_dedup_guard: bool) -> bool {
        let Err(err) = &self.inner else { return false };
        if !self.callbacks_report.is_empty() || !self.progress.can_retry() || !err.is_transient() {
            return false;
        }
        has_dedup_guard || !matches!(err, IntentExecutorError::FailedExecution(_))
    }

    pub fn is_err(&self) -> bool {
        self.inner.is_err()
    }

    pub fn is_ok(&self) -> bool {
        self.inner.is_ok()
    }
}

#[derive(Default)]
pub struct IntentExecutionReport {
    pub progress: ExecutionProgress,
    /// Resource ownership from each preparation attempt, including failed attempts.
    cleanup: Vec<CleanupResources>,
    /// Errors we patched trying to recover intent
    patched_errors: Vec<TransactionExecutionError>,
    /// Report of scheduled callbacks
    callbacks_report: Vec<Result<Signature, CallbackScheduleError>>,
}

impl IntentExecutionReport {
    pub fn add_patched_error(&mut self, value: TransactionExecutionError) {
        self.patched_errors.push(value);
    }

    pub fn patched_errors(&self) -> &[TransactionExecutionError] {
        &self.patched_errors
    }

    pub fn callbacks_report(&self) -> &[Result<Signature, CallbackScheduleError>] {
        &self.callbacks_report
    }

    pub fn add_callback_report(
        &mut self,
        values: impl IntoIterator<Item = Result<Signature, CallbackScheduleError>>,
    ) {
        self.callbacks_report.extend(values);
    }
}
