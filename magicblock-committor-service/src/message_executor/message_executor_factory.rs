use magicblock_rpc_client::MagicblockRpcClient;
use magicblock_table_mania::TableMania;

use crate::{
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
}

impl MessageExecutorFactory for L1MessageExecutorFactory {
    type Executor = L1MessageExecutor<TransactionPreparatorV1>;

    fn create_instance(&self) -> Self::Executor {
        L1MessageExecutor::<TransactionPreparatorV1>::new_v1(
            self.rpc_client.clone(),
            self.table_mania.clone(),
            self.compute_budget_config.clone(),
        )
    }
}
