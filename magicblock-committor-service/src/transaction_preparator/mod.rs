use async_trait::async_trait;
use magicblock_rpc_client::MagicblockRpcClient;
use magicblock_table_mania::TableMania;
use solana_sdk::{message::VersionedMessage, signature::Keypair};

use crate::{
    persist::IntentPersister,
    tasks::{task_strategist::TransactionStrategy, utils::TransactionUtils},
    transaction_preparator::{
        delivery_preparator::DeliveryPreparator, error::PreparatorResult,
    },
    ComputeBudgetConfig,
};

pub mod delivery_preparator;
pub mod error;

#[async_trait]
pub trait TransactionPreparator: Send + Sync + 'static {
    /// Return [`VersionedMessage`] corresponding to [`TransactionStrategy`]
    /// Handles all necessary preparation needed for successful [`BaseTask`] execution
    async fn prepare_for_strategy<P: IntentPersister>(
        &self,
        authority: &Keypair,
        transaction_strategy: &TransactionStrategy,
        intent_persister: &Option<P>,
    ) -> PreparatorResult<VersionedMessage>;
}

/// [`TransactionPreparatorV1`] first version of preparator
/// It omits future commit_bundle/finalize_bundle logic
/// It creates TXs using current per account commit/finalize
pub struct TransactionPreparatorV1 {
    delivery_preparator: DeliveryPreparator,
    compute_budget_config: ComputeBudgetConfig,
}

impl TransactionPreparatorV1 {
    pub fn new(
        rpc_client: MagicblockRpcClient,
        table_mania: TableMania,
        compute_budget_config: ComputeBudgetConfig,
    ) -> Self {
        let delivery_preparator = DeliveryPreparator::new(
            rpc_client.clone(),
            table_mania,
            compute_budget_config.clone(),
        );

        Self {
            delivery_preparator,
            compute_budget_config,
        }
    }
}

#[async_trait]
impl TransactionPreparator for TransactionPreparatorV1 {
    async fn prepare_for_strategy<P: IntentPersister>(
        &self,
        authority: &Keypair,
        tx_strategy: &TransactionStrategy,
        intent_persister: &Option<P>,
    ) -> PreparatorResult<VersionedMessage> {
        // If message won't fit, there's no reason to prepare anything
        // Fail early
        {
            let dummy_lookup_tables = TransactionUtils::dummy_lookup_table(
                &tx_strategy.lookup_tables_keys,
            );
            let _ = TransactionUtils::assemble_tasks_tx(
                authority,
                &tx_strategy.optimized_tasks,
                self.compute_budget_config.compute_unit_price,
                &dummy_lookup_tables,
            )?;
        }

        // Pre tx preparations. Create buffer accs + lookup tables
        let lookup_tables = self
            .delivery_preparator
            .prepare_for_delivery(authority, tx_strategy, intent_persister)
            .await?;

        let message = TransactionUtils::assemble_tasks_tx(
            authority,
            &tx_strategy.optimized_tasks,
            self.compute_budget_config.compute_unit_price,
            &lookup_tables,
        )
        .expect("Possibility to assemble checked above")
        .message;

        Ok(message)
    }
}
