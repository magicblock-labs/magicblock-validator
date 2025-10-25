use std::sync::Arc;

use async_trait::async_trait;
use light_client::indexer::photon_indexer::PhotonIndexer;
use magicblock_rpc_client::MagicblockRpcClient;
use magicblock_table_mania::TableMania;
use solana_pubkey::Pubkey;
use solana_sdk::{message::VersionedMessage, signature::Keypair};

use crate::{
    persist::IntentPersister,
    tasks::{
        task_strategist::TransactionStrategy, utils::TransactionUtils, BaseTask,
    },
    transaction_preparator::{
        delivery_preparator::{
            DeliveryPreparator, DeliveryPreparatorResult, InternalError,
        },
        error::PreparatorResult,
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
        transaction_strategy: &mut TransactionStrategy,
        intent_persister: &Option<P>,
        photon_client: &Option<Arc<PhotonIndexer>>,
    ) -> PreparatorResult<VersionedMessage>;

    /// Cleans up after strategy
    async fn cleanup_for_strategy(
        &self,
        authority: &Keypair,
        tasks: &[Box<dyn BaseTask>],
        lookup_table_keys: &[Pubkey],
    ) -> DeliveryPreparatorResult<(), InternalError>;
}

/// [`TransactionPreparatorImpl`] first version of preparator
/// It omits future commit_bundle/finalize_bundle logic
/// It creates TXs using current per account commit/finalize
#[derive(Clone)]
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
    async fn prepare_for_strategy<P: IntentPersister>(
        &self,
        authority: &Keypair,
        tx_strategy: &mut TransactionStrategy,
        intent_persister: &Option<P>,
        photon_client: &Option<Arc<PhotonIndexer>>,
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
            .prepare_for_delivery(
                authority,
                tx_strategy,
                intent_persister,
                photon_client,
            )
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

    async fn cleanup_for_strategy(
        &self,
        authority: &Keypair,
        tasks: &[Box<dyn BaseTask>],
        lookup_table_keys: &[Pubkey],
    ) -> DeliveryPreparatorResult<(), InternalError> {
        self.delivery_preparator
            .cleanup(authority, tasks, lookup_table_keys)
            .await
    }
}
