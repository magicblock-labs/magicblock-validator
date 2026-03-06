use std::sync::Arc;

use magicblock_rpc_client::MagicblockRpcClient;
use magicblock_table_mania::TableMania;

use crate::{
    intent_executor::{
        task_info_fetcher::{CacheTaskInfoFetcher, RpcTaskInfoFetcher},
        IntentExecutor, IntentExecutorImpl,
    },
    transaction_preparator::TransactionPreparatorImpl,
    ComputeBudgetConfig,
};

pub trait IntentExecutorFactory {
    type Executor: IntentExecutor;

    fn create_instance(&self) -> Self::Executor;
}

/// Dummy struct to simplify signature of CommitSchedulerWorker
pub struct IntentExecutorFactoryImpl {
    pub rpc_client: MagicblockRpcClient,
    pub table_mania: TableMania,
    pub compute_budget_config: ComputeBudgetConfig,
    pub task_info_fetcher: Arc<CacheTaskInfoFetcher<RpcTaskInfoFetcher>>,
}

impl IntentExecutorFactory for IntentExecutorFactoryImpl {
    type Executor =
        IntentExecutorImpl<TransactionPreparatorImpl, RpcTaskInfoFetcher>;

    fn create_instance(&self) -> Self::Executor {
        let transaction_preparator = TransactionPreparatorImpl::new(
            self.rpc_client.clone(),
            self.table_mania.clone(),
            self.compute_budget_config.clone(),
        );
        IntentExecutorImpl::<TransactionPreparatorImpl, RpcTaskInfoFetcher>::new(
            self.rpc_client.clone(),
            transaction_preparator,
            self.task_info_fetcher.clone(),
        )
    }
}
