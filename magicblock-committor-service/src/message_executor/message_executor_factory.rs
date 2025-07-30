use std::sync::Arc;

use magicblock_rpc_client::MagicblockRpcClient;
use magicblock_table_mania::TableMania;

use crate::{
    commit_scheduler::commit_id_tracker::CommitIdTrackerImpl,
    message_executor::{L1MessageExecutor, MessageExecutor},
    transaction_preperator::transaction_preparator::TransactionPreparatorV1,
    ComputeBudgetConfig,
};

pub trait MessageExecutorFactory {
    type Executor: MessageExecutor;

    fn create_instance(&self) -> Self::Executor;
}

/// Dummy struct to simplify signature of CommitSchedulerWorker
pub struct L1MessageExecutorFactory {
    pub rpc_client: MagicblockRpcClient,
    pub table_mania: TableMania,
    pub compute_budget_config: ComputeBudgetConfig,
    pub commit_id_tracker: Arc<CommitIdTrackerImpl>,
}

impl MessageExecutorFactory for L1MessageExecutorFactory {
    type Executor =
        L1MessageExecutor<TransactionPreparatorV1<CommitIdTrackerImpl>>;

    fn create_instance(&self) -> Self::Executor {
        let transaction_preaparator =
            TransactionPreparatorV1::<CommitIdTrackerImpl>::new(
                self.rpc_client.clone(),
                self.table_mania.clone(),
                self.compute_budget_config.clone(),
                self.commit_id_tracker.clone(),
            );
        L1MessageExecutor::<TransactionPreparatorV1<CommitIdTrackerImpl>>::new(
            self.rpc_client.clone(),
            transaction_preaparator,
        )
    }
}
