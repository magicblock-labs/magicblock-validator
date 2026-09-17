use std::{sync::Arc, time::Duration};

use magicblock_core::traits::ActionsCallbackScheduler;
use magicblock_program::outbox_intent_bundles::OutboxIntentBundleStatus;
use magicblock_rpc_client::MagicblockRpcClient;
use magicblock_table_mania::TableMania;
use solana_keypair::Keypair;

use crate::{
    ComputeBudgetConfig,
    intent_executor::{
        IntentExecutor, IntentExecutorCtx, build_stage_intent_executor,
        error::IntentExecutorError,
        intent_execution_client::IntentExecutionClient,
    },
    outbox::OutboxClient,
    tasks::task_info_fetcher::{CacheTaskInfoFetcher, RpcTaskInfoFetcher},
    transaction_preparator::TransactionPreparatorImpl,
};

pub trait IntentExecutorBuilder<TPreparator> {
    fn create_instance(
        &self,
        status: OutboxIntentBundleStatus,
    ) -> Box<dyn IntentExecutor<TPreparator>>;
}

pub struct ExecutorConfig {
    pub compute_budget_config: ComputeBudgetConfig,
    pub actions_timeout: Duration,
}

/// Dummy struct to simplify signature of IntentExecutionEngine
pub struct IntentExecutorBuilderImpl<TCallbackScheduler, TOutbox> {
    /// Base-layer signing identity — the engine's authority keypair.
    pub authority: Keypair,
    pub rpc_client: MagicblockRpcClient,
    pub table_mania: TableMania,
    pub executor_config: ExecutorConfig,
    pub task_info_fetcher: Arc<CacheTaskInfoFetcher<RpcTaskInfoFetcher>>,
    pub outbox_client: Arc<TOutbox>,
    pub actions_callback_executor: TCallbackScheduler,
}

impl<TCallbackScheduler, TOutbox>
    IntentExecutorBuilder<TransactionPreparatorImpl>
    for IntentExecutorBuilderImpl<TCallbackScheduler, TOutbox>
where
    TCallbackScheduler: ActionsCallbackScheduler,
    TOutbox: OutboxClient,
    TOutbox::Error: Into<IntentExecutorError>,
{
    fn create_instance(
        &self,
        status: OutboxIntentBundleStatus,
    ) -> Box<dyn IntentExecutor<TransactionPreparatorImpl>> {
        let transaction_preparator = TransactionPreparatorImpl::new(
            self.rpc_client.clone(),
            self.table_mania.clone(),
            self.executor_config.compute_budget_config.clone(),
        );
        let ctx = IntentExecutorCtx {
            authority: self.authority.insecure_clone(),
            intent_client: IntentExecutionClient::new(self.rpc_client.clone()),
            transaction_preparator,
            task_info_fetcher: self.task_info_fetcher.clone(),
            outbox_client: self.outbox_client.clone(),
            actions_callback_executor: self.actions_callback_executor.clone(),
        };
        build_stage_intent_executor(
            ctx,
            status,
            self.executor_config.actions_timeout,
        )
    }
}
