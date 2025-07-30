pub mod error;
pub mod message_executor;
pub(crate) mod message_executor_factory;

use async_trait::async_trait;
use magicblock_program::magic_scheduled_l1_message::ScheduledL1Message;
pub use message_executor::L1MessageExecutor;
use solana_sdk::signature::Signature;

use crate::{
    message_executor::error::MessageExecutorResult,
    persist::L1MessagesPersisterIface,
};

#[derive(Clone, Debug)]
pub struct ExecutionOutput {
    /// Commit stage signature
    pub commit_signature: Signature,
    /// Finalize stage signature
    pub finalize_signature: Signature,
}

#[async_trait]
pub trait MessageExecutor: Send + Sync + 'static {
    /// Executes Message on Base layer
    /// Returns `ExecutionOutput` or an `Error`
    async fn execute<P: L1MessagesPersisterIface>(
        &self,
        l1_message: ScheduledL1Message,
        persister: Option<P>,
    ) -> MessageExecutorResult<ExecutionOutput>;
}
