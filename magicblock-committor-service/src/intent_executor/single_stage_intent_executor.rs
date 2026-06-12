use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use async_trait::async_trait;
use magicblock_core::traits::ActionsCallbackScheduler;
use magicblock_program::{
    magic_scheduled_base_intent::ScheduledIntentBundle,
    validator::validator_authority,
};
use magicblock_rpc_client::MagicblockRpcClient;
use solana_keypair::Keypair;
use solana_signature::Signature;
use solana_signer::Signer;

use crate::{
    intent_executor::{
        accepted_intent_executor::AcceptedIntentExecutor,
        cleanup_handle::CleanupHandle,
        error::IntentExecutorError,
        intent_execution_client::IntentExecutionClient,
        task_info_fetcher::{CacheTaskInfoFetcher, TaskInfoFetcher},
        IntentExecutionResult, IntentExecutor, IntentExecutorCtx,
    },
    outbox_client::OutboxClient,
    tasks::task_strategist::TransactionStrategy,
    transaction_preparator::TransactionPreparator,
};

pub struct SingleStageIntentExecutor<T, F, A, O> {
    authority: Keypair,
    /// Signature of committing
    pending_signature: Signature,
    /// Intent Executor context
    ctx: IntentExecutorCtx<T, F, A, O>,

    /// Intent execution started at
    pub started_at: Instant,
    /// Junk that needs to be cleaned up
    junk: Vec<TransactionStrategy>,
    /// Set to false on execution failure so cleanup only releases ALT
    /// reservations without closing buffer PDAs (see race condition note in
    /// intent_execution_engine)
    close_buffers: bool,
}

impl<T, F, A, O> SingleStageIntentExecutor<T, F, A, O>
where
    T: TransactionPreparator,
    F: TaskInfoFetcher,
    A: ActionsCallbackScheduler,
    O: OutboxClient,
    O::Error: Into<IntentExecutorError>,
{
    pub fn new(
        ctx: IntentExecutorCtx<T, F, A, O>,
        pending_signature: Signature,
    ) -> Self {
        let authority = validator_authority();
        Self {
            authority,
            pending_signature,
            ctx,

            started_at: Instant::now(),
            junk: vec![],
            close_buffers: true,
        }
    }
}

#[async_trait]
impl<T, F, A, O> IntentExecutor<T> for SingleStageIntentExecutor<T, F, A, O>
where
    T: TransactionPreparator,
    F: TaskInfoFetcher,
    A: ActionsCallbackScheduler,
    O: OutboxClient,
    O::Error: Into<IntentExecutorError>,
{
    async fn execute(
        mut self: Box<Self>,
        base_intent: ScheduledIntentBundle,
    ) -> (IntentExecutionResult, CleanupHandle<T>) {
        todo!()
    }
}
