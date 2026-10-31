use async_trait::async_trait;
use metrics::metrics;
use solana_keypair::Keypair;
use solana_message::VersionedMessage;

use crate::{
    execution::preparation::{
        delivery_preparator::{BufferExecutionError, DeliveryPreparatorResult},
        error::PreparatorResult,
    },
    shared::tasks::{task_strategist::TransactionStrategy, utils::TransactionUtils},
};

pub(crate) mod cleanup_resources;
pub(crate) mod delivery_preparator;
pub(crate) mod error;

use cleanup_resources::CleanupResources;
pub(crate) use delivery_preparator::Preparator;

#[async_trait]
pub(crate) trait TransactionPreparator: Send + Sync + 'static {
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

#[async_trait]
impl TransactionPreparator for Preparator {
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
            self.prepare_for_delivery(authority, tx_strategy, cleanup).await?;
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
        let lookup_tables = self.prepare_for_delivery(authority, tx_strategy, cleanup).await?;
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
        self.cleanup(authority, resources, close_buffers).await
    }
}
