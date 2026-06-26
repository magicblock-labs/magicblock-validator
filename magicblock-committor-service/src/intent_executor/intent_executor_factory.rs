use std::{sync::Arc, time::Duration};

use magicblock_core::traits::ActionsCallbackScheduler;
use magicblock_program::outbox_intent_bundles::OutboxIntentBundleStatus;
use magicblock_rpc_client::MagicblockRpcClient;
use magicblock_table_mania::TableMania;

use crate::{
    intent_executor::{
        build_stage_intent_executor, error::IntentExecutorError,
        intent_execution_client::IntentExecutionClient, IntentExecutor,
        IntentExecutorCtx,
    },
    outbox_client::OutboxClient,
    tasks::task_info_fetcher::{CacheTaskInfoFetcher, RpcTaskInfoFetcher},
    transaction_preparator::TransactionPreparatorImpl,
    ComputeBudgetConfig,
};

// TODO(edwin): T could be removed if cleanuphandle is a trait
pub trait IntentExecutorBuilder<T> {
    fn create_instance(
        &self,
        status: OutboxIntentBundleStatus,
    ) -> Box<dyn IntentExecutor<T>>;
}

pub struct ExecutorConfig {
    pub compute_budget_config: ComputeBudgetConfig,
    pub actions_timeout: Duration,
}

/// Dummy struct to simplify signature of CommitSchedulerWorker
pub struct IntentExecutorBuilderImpl<A, O> {
    pub rpc_client: MagicblockRpcClient,
    pub table_mania: TableMania,
    pub executor_config: ExecutorConfig,
    pub task_info_fetcher: Arc<CacheTaskInfoFetcher<RpcTaskInfoFetcher>>,
    pub outbox_client: Arc<O>,
    pub actions_callback_executor: A,
}

impl<A, O> IntentExecutorBuilder<TransactionPreparatorImpl>
    for IntentExecutorBuilderImpl<A, O>
where
    A: ActionsCallbackScheduler,
    O: OutboxClient,
    O::Error: Into<IntentExecutorError>,
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
            intent_client: IntentExecutionClient::new(self.rpc_client.clone()),
            transaction_preparator,
            task_info_fetcher: self.task_info_fetcher.clone(),
            outbox_client: self.outbox_client.clone(),
            actions_callback_executor: self.actions_callback_executor.clone(),
            actions_timeout: self.executor_config.actions_timeout,
        };
        build_stage_intent_executor(ctx, status)
    }
}
