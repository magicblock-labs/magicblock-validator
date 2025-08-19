pub mod error;
#[allow(clippy::module_inception)]
pub mod intent_executor;
pub(crate) mod intent_executor_factory;
pub mod task_info_fetcher;

use async_trait::async_trait;
pub use intent_executor::IntentExecutorImpl;
use magicblock_program::magic_scheduled_base_intent::ScheduledBaseIntent;
use solana_sdk::signature::Signature;

use crate::{
    intent_executor::error::IntentExecutorResult, persist::IntentPersister,
};

#[derive(Clone, Debug)]
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

#[async_trait]
pub trait IntentExecutor: Send + Sync + 'static {
    /// Executes Message on Base layer
    /// Returns `ExecutionOutput` or an `Error`
    async fn execute<P: IntentPersister>(
        &self,
        l1_message: ScheduledBaseIntent,
        persister: Option<P>,
    ) -> IntentExecutorResult<ExecutionOutput>;
}
