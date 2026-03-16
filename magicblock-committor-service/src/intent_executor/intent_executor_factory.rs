use std::{sync::Arc, time::Duration};

use magicblock_core::traits::ActionsCallbackExecutor;
use magicblock_rpc_client::MagicblockRpcClient;
use magicblock_table_mania::TableMania;
use solana_signer::SignerError;

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

pub struct ExecutorConfig {
    pub compute_budget_config: ComputeBudgetConfig,
    pub actions_timeout: Duration,
}

/// Dummy struct to simplify signature of CommitSchedulerWorker
pub struct IntentExecutorFactoryImpl<A> {
    pub rpc_client: MagicblockRpcClient,
    pub table_mania: TableMania,
    pub executor_config: ExecutorConfig,
    pub task_info_fetcher: Arc<CacheTaskInfoFetcher<RpcTaskInfoFetcher>>,
    pub actions_callback_executor: A,
}

impl<A> IntentExecutorFactory for IntentExecutorFactoryImpl<A>
where
    A: ActionsCallbackExecutor<ScheduleError = SignerError>,
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
            self.rpc_client.clone(),
            transaction_preparator,
            self.task_info_fetcher.clone(),
            self.actions_callback_executor.clone(),
            self.executor_config.actions_timeout,
        )
    }
}
