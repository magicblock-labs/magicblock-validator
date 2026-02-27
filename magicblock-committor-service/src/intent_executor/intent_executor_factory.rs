use std::{sync::Arc, time::Duration};

use magicblock_rpc_client::MagicblockRpcClient;
use magicblock_table_mania::TableMania;

use crate::{
    intent_executor::{
        task_info_fetcher::CacheTaskInfoFetcher, ActionsCallbackExecutor,
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
pub struct IntentExecutorFactoryImpl<A> {
    pub rpc_client: MagicblockRpcClient,
    pub table_mania: TableMania,
    pub compute_budget_config: ComputeBudgetConfig,
    pub commit_id_tracker: Arc<CacheTaskInfoFetcher>,
    pub actions_callback_executor: A,
    pub actions_timeout: Duration,
}

impl<A: ActionsCallbackExecutor> IntentExecutorFactory
    for IntentExecutorFactoryImpl<A>
{
    type Executor = IntentExecutorImpl<
        TransactionPreparatorImpl,
        CacheTaskInfoFetcher,
        A,
    >;

    fn create_instance(&self) -> Self::Executor {
        let transaction_preparator = TransactionPreparatorImpl::new(
            self.rpc_client.clone(),
            self.table_mania.clone(),
            self.compute_budget_config.clone(),
        );
        IntentExecutorImpl::new(
            self.rpc_client.clone(),
            transaction_preparator,
            self.commit_id_tracker.clone(),
            self.actions_callback_executor.clone(),
            self.actions_timeout,
        )
    }
}
