use std::{sync::Arc, time::Duration};

use magicblock_core::traits::ActionsCallbackScheduler;
use magicblock_rpc_client::MagicblockRpcClient;
use magicblock_table_mania::TableMania;
use solana_keypair::Keypair;

use crate::{
    ComputeBudgetConfig,
    intent_executor::{
        IntentExecutor, IntentExecutorImpl,
        task_info_fetcher::{CacheTaskInfoFetcher, RpcTaskInfoFetcher},
    },
    transaction_preparator::TransactionPreparatorImpl,
};

pub trait IntentExecutorFactory {
    type Executor: IntentExecutor;

    fn create_instance(&self) -> Self::Executor;
}

pub struct ExecutorConfig {
    pub compute_budget_config: ComputeBudgetConfig,
    pub actions_timeout: Duration,
}

/// Dummy struct to simplify signature of CommitSchedulerWorker
pub struct IntentExecutorFactoryImpl<A> {
    /// Base-layer signing identity — the engine's authority keypair.
    pub authority: Keypair,
    pub rpc_client: MagicblockRpcClient,
    pub table_mania: TableMania,
    pub executor_config: ExecutorConfig,
    pub task_info_fetcher: Arc<CacheTaskInfoFetcher<RpcTaskInfoFetcher>>,
    pub actions_callback_executor: A,
}

impl<A> IntentExecutorFactory for IntentExecutorFactoryImpl<A>
where
    A: ActionsCallbackScheduler,
{
    type Executor =
        IntentExecutorImpl<TransactionPreparatorImpl, RpcTaskInfoFetcher, A>;

    fn create_instance(&self) -> Self::Executor {
        let transaction_preparator = TransactionPreparatorImpl::new(
            self.rpc_client.clone(),
            self.table_mania.clone(),
            self.executor_config.compute_budget_config.clone(),
        );
        Self::Executor::new(
            self.authority.insecure_clone(),
            self.rpc_client.clone(),
            transaction_preparator,
            self.task_info_fetcher.clone(),
            self.actions_callback_executor.clone(),
            self.executor_config.actions_timeout,
        )
    }
}
