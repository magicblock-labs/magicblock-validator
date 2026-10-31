use async_trait::async_trait;
use metrics::metrics;
use rpc_client::MagicblockRpcClient;
use solana_keypair::Keypair;
use solana_message::VersionedMessage;
use table_mania::TableMania;

use crate::{
    ComputeBudgetConfig,
    tasks::{task_strategist::TransactionStrategy, utils::TransactionUtils},
    transaction_preparator::{
        delivery_preparator::{BufferExecutionError, DeliveryPreparator, DeliveryPreparatorResult},
        error::PreparatorResult,
    },
};

pub mod cleanup_resources;
pub mod delivery_preparator;
pub mod error;

use cleanup_resources::CleanupResources;

#[async_trait]
pub trait TransactionPreparator: Send + Sync + 'static {
    /// Return [`VersionedMessage`] corresponding to [`TransactionStrategy`]
    /// Handles all preparation needed to execute the strategy's tasks.
    async fn prepare_for_strategy(
        &self,
        authority: &Keypair,
        transaction_strategy: &TransactionStrategy,
        cleanup: &mut CleanupResources,
    ) -> PreparatorResult<VersionedMessage>;

    /// Releases the resources acquired by one preparation attempt.
    /// `close_buffers`: if false, only ALT reservations are released.
    async fn cleanup_resources(
        &self,
        authority: &Keypair,
        resources: &CleanupResources,
        close_buffers: bool,
    ) -> DeliveryPreparatorResult<(), BufferExecutionError>;
}

/// [`TransactionPreparatorImpl`] first version of preparator
/// It omits future commit_bundle/finalize_bundle logic
/// It creates TXs using current per account commit/finalize
pub struct TransactionPreparatorImpl {
    delivery_preparator: DeliveryPreparator,
    compute_budget_config: ComputeBudgetConfig,
}

impl TransactionPreparatorImpl {
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
impl TransactionPreparator for TransactionPreparatorImpl {
    async fn prepare_for_strategy(
        &self,
        authority: &Keypair,
        tx_strategy: &TransactionStrategy,
        cleanup: &mut CleanupResources,
    ) -> PreparatorResult<VersionedMessage> {
        if !tx_strategy.uses_alts() {
            // Validate the final representation before preparing buffers.
            let message = TransactionUtils::assemble_tasks_v1_tx_with_uniqueness_nonce(
                authority,
                &tx_strategy.tasks,
                self.compute_budget_config.compute_unit_price,
                tx_strategy.uniqueness_nonce,
            )?
            .message;
            self.delivery_preparator
                .prepare_for_delivery(authority, tx_strategy, cleanup)
                .await?;
            metrics::observe_committor_intent_alt_count(0);
            return Ok(message);
        }

        // If message won't fit, there's no reason to prepare anything
        // Fail early
        {
            let dummy_lookup_tables =
                TransactionUtils::dummy_lookup_table(&tx_strategy.lookup_tables_keys);
            let _ = TransactionUtils::assemble_tasks_tx_with_uniqueness_nonce(
                authority,
                &tx_strategy.tasks,
                self.compute_budget_config.compute_unit_price,
                &dummy_lookup_tables,
                tx_strategy.uniqueness_nonce,
            )?;
        }

        // Pre tx preparations. Create buffer accs + lookup tables
        let lookup_tables = self
            .delivery_preparator
            .prepare_for_delivery(authority, tx_strategy, cleanup)
            .await?;
        metrics::observe_committor_intent_alt_count(lookup_tables.len());

        let message = TransactionUtils::assemble_tasks_tx_with_uniqueness_nonce(
            authority,
            &tx_strategy.tasks,
            self.compute_budget_config.compute_unit_price,
            &lookup_tables,
            tx_strategy.uniqueness_nonce,
        )?
        .message;

        Ok(message)
    }

    async fn cleanup_resources(
        &self,
        authority: &Keypair,
        resources: &CleanupResources,
        close_buffers: bool,
    ) -> DeliveryPreparatorResult<(), BufferExecutionError> {
        self.delivery_preparator.cleanup(authority, resources, close_buffers).await
    }
}
