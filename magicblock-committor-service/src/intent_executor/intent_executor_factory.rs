use std::sync::Arc;

use magicblock_rpc_client::MagicblockRpcClient;
use magicblock_table_mania::TableMania;

use crate::{
    intent_executor::{
        commit_id_fetcher::CommitIdTrackerImpl, IntentExecutor,
        IntentExecutorImpl,
    },
    transaction_preperator::transaction_preparator::TransactionPreparatorV1,
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
    pub commit_id_tracker: Arc<CommitIdTrackerImpl>,
}

impl IntentExecutorFactory for IntentExecutorFactoryImpl {
    type Executor =
        IntentExecutorImpl<TransactionPreparatorV1<CommitIdTrackerImpl>>;

    fn create_instance(&self) -> Self::Executor {
        let transaction_preaparator =
            TransactionPreparatorV1::<CommitIdTrackerImpl>::new(
                self.rpc_client.clone(),
                self.table_mania.clone(),
                self.compute_budget_config.clone(),
                self.commit_id_tracker.clone(),
            );
        IntentExecutorImpl::<TransactionPreparatorV1<CommitIdTrackerImpl>>::new(
            self.rpc_client.clone(),
            transaction_preaparator,
        )
    }
}
