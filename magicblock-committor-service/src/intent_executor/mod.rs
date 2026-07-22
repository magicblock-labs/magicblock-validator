pub mod accepted_intent_executor;
pub mod cleanup_handle;
pub mod error;
pub mod intent_execution_client;
pub(crate) mod intent_executor_factory;
pub mod single_stage_intent_executor;
pub mod strategy_executor;
pub mod two_stage_intent_executor;
pub mod utils;

use std::{sync::Arc, time::Duration};

use async_trait::async_trait;
use magicblock_core::traits::{
    ActionsCallbackScheduler, CallbackScheduleError,
};
use magicblock_metrics::metrics;
use magicblock_program::{
    magic_scheduled_base_intent::ScheduledIntentBundle, outbox::ExecutionStage,
    outbox_intent_bundles::OutboxIntentBundleStatus,
};
use solana_signature::Signature;
use strategy_executor::error::TransactionStrategyExecutionError;

use crate::{
    intent_executor::{
        accepted_intent_executor::AcceptedIntentExecutor,
        cleanup_handle::CleanupHandle,
        error::{IntentExecutorError, IntentExecutorResult},
        intent_execution_client::IntentExecutionClient,
        single_stage_intent_executor::SingleStageIntentExecutor,
        two_stage_intent_executor::TwoStageIntentExecutor,
    },
    outbox::OutboxClient,
    tasks::{
        task_info_fetcher::{CacheTaskInfoFetcher, TaskInfoFetcher},
        task_strategist::TransactionStrategy,
    },
    transaction_preparator::TransactionPreparator,
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

pub fn build_stage_intent_executor<T, F, A, O>(
    ctx: IntentExecutorCtx<T, F, A, O>,
    status: OutboxIntentBundleStatus,
    actions_timeout: Duration,
) -> Box<dyn IntentExecutor<T>>
where
    T: TransactionPreparator,
    F: TaskInfoFetcher,
    A: ActionsCallbackScheduler,
    O: OutboxClient,
    O::Error: Into<IntentExecutorError>,
{
    match status {
        OutboxIntentBundleStatus::Accepted => {
            Box::new(AcceptedIntentExecutor::new(ctx, actions_timeout))
                as Box<dyn IntentExecutor<T> + 'static>
        }
        OutboxIntentBundleStatus::Executing(ExecutionStage::SingleStage(
            sig,
        )) => {
            Box::new(SingleStageIntentExecutor::new(ctx, actions_timeout, sig))
                as Box<dyn IntentExecutor<T> + 'static>
        }
        OutboxIntentBundleStatus::Executing(ExecutionStage::TwoStage(
            value,
        )) => {
            Box::new(TwoStageIntentExecutor::new(ctx, actions_timeout, value))
                as Box<dyn IntentExecutor<T> + 'static>
        }
    }
}

pub struct IntentExecutorCtx<T, F, A, O> {
    pub intent_client: IntentExecutionClient,
    pub transaction_preparator: T,
    pub task_info_fetcher: Arc<CacheTaskInfoFetcher<F>>,
    pub outbox_client: Arc<O>,
    pub actions_callback_executor: A,
}

#[derive(Clone, Copy, Debug)]
pub enum ExecutionOutput {
    // TODO: with arrival of challenge window remove SingleStage
    // Protocol requires 2 stage: Commit, Finalize
    // SingleStage - optimization for timebeing
    SingleStage(Signature),
    TwoStage {
        /// Commit stage signature
        commit_signature: Signature,
        /// Finalize stage signature
        finalize_signature: Signature,
    },
}

impl metrics::LabelValue for ExecutionOutput {
    fn value(&self) -> &str {
        match self {
            Self::SingleStage(_) => "single_stage_succeeded",
            Self::TwoStage {
                commit_signature: _,
                finalize_signature: _,
            } => "two_stage_succeeded",
        }
    }
}

pub struct IntentExecutionResult {
    /// Final result of Intent Execution
    pub inner: IntentExecutorResult<ExecutionOutput>,
    /// Errors patched along the way
    pub patched_errors: Vec<TransactionStrategyExecutionError>,
    /// Callbacks result
    pub callbacks_report: Vec<Result<Signature, CallbackScheduleError>>,
    #[cfg(feature = "dev-context-only-utils")]
    /// Strategies that were successfully executed (test only)
    pub successful_transaction_strategies: Vec<TransactionStrategy>,
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
        let send_stage_failure = matches!(
            &self.inner,
            Err(IntentExecutorError::FailedToCommitError { .. })
                | Err(IntentExecutorError::FailedToFinalizeError { .. })
        );
        self.callbacks_report.is_empty()
            && matches!(&self.inner, Err(err) if err.is_transient())
            && (has_dedup_guard || !send_stage_failure)
    }
}

#[derive(Default)]
pub struct IntentExecutionReport {
    /// Junk that needs to be cleaned up
    junk: Vec<TransactionStrategy>,
    /// Errors we patched trying to recover intent
    patched_errors: Vec<TransactionStrategyExecutionError>,
    /// Report of scheduled callbacks
    callbacks_report: Vec<Result<Signature, CallbackScheduleError>>,
    #[cfg(feature = "dev-context-only-utils")]
    /// Succeeded transaction strategies report (test only)
    successful_transaction_strategies: Vec<TransactionStrategy>,
}

impl IntentExecutionReport {
    pub fn dispose(&mut self, value: TransactionStrategy) {
        self.junk.push(value);
    }

    pub fn add_patched_error(
        &mut self,
        value: TransactionStrategyExecutionError,
    ) {
        self.patched_errors.push(value);
    }

    pub fn patched_errors(&self) -> &[TransactionStrategyExecutionError] {
        &self.patched_errors
    }

    pub fn callbacks_report(
        &self,
    ) -> &[Result<Signature, CallbackScheduleError>] {
        &self.callbacks_report
    }

    pub fn add_callback_report(
        &mut self,
        values: impl IntoIterator<Item = Result<Signature, CallbackScheduleError>>,
    ) {
        self.callbacks_report.extend(values);
    }

    pub fn junk(&self) -> &Vec<TransactionStrategy> {
        &self.junk
    }

    #[cfg(feature = "dev-context-only-utils")]
    pub fn add_succeeded_transaction_strategy(
        &mut self,
        value: TransactionStrategy,
    ) {
        self.successful_transaction_strategies.push(value);
    }
}
