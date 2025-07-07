use std::future::Future;

use futures_util::future::{join, join_all};
use magicblock_rpc_client::MagicblockRpcClient;
use magicblock_table_mania::TableMania;
use solana_sdk::message::AddressLookupTableAccount;

use crate::transaction_preperator::{
    delivery_strategist::{TaskDeliveryStrategy, TransactionStrategy},
    error::PreparatorResult,
    task_builder::Task,
};

type PreparationFuture = impl Future<Output = PreparatorResult<()>>;

pub struct DeliveryPreparationResult {
    lookup_tables: Vec<AddressLookupTableAccount>,
}

pub struct DeliveryPreparator {
    rpc_client: MagicblockRpcClient,
    table_mania: TableMania,
}

impl DeliveryPreparator {
    pub fn new(
        rpc_client: MagicblockRpcClient,
        table_mania: TableMania,
    ) -> Self {
        Self {
            rpc_client,
            table_mania,
        }
    }

    /// Prepares buffers and necessary pieces for optimized TX
    pub async fn prepare_for_delivery(
        &self,
        strategy: &TransactionStrategy,
    ) -> DeliveryPreparationResult {
        let preparation_futures = strategy
            .task_strategies
            .iter()
            .filter_map(|strategy| match strategy {
                TaskDeliveryStrategy::Args(_) => None,
                TaskDeliveryStrategy::Buffer(task) => Some(task),
            })
            .map(|task| self.prepare_buffer(task));
        // .collect::<Vec<impl Future<Output = PreparatorResult<()>>>>();

        join_all(preparation_futures);
        join()
    }

    async fn prepare_buffer(&self, task: &Task) -> PreparatorResult<()> {
        todo!();
        Ok(())
    }

    async fn prepare_lookup_tables(
        &self,
        strategies: Vec<TaskDeliveryStrategy>,
    ) -> PreparatorResult<Vec<AddressLookupTableAccount>> {
        // self.table_mania.
        todo!()
    }
}
