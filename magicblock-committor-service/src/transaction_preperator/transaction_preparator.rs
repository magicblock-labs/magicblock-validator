use std::{fmt::Formatter, sync::Arc};

use async_trait::async_trait;
use magicblock_program::magic_scheduled_base_intent::ScheduledBaseIntent;
use magicblock_rpc_client::MagicblockRpcClient;
use magicblock_table_mania::TableMania;
use solana_sdk::{
    message::VersionedMessage, signature::Keypair, signer::Signer,
};

use crate::{
    intent_executor::commit_id_fetcher::CommitIdFetcher,
    persist::IntentPersister,
    tasks::{
        task_builder::{TaskBuilderV1, TasksBuilder},
        task_strategist::TaskStrategist,
        utils::TransactionUtils,
    },
    transaction_preperator::{
        delivery_preparator::DeliveryPreparator, error::PreparatorResult,
    },
    ComputeBudgetConfig,
};

/// Transaction Preparator version
/// Some actions maybe invalid per version
#[derive(Debug)]
pub enum PreparatorVersion {
    V1,
}

impl std::fmt::Display for PreparatorVersion {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::V1 => write!(f, "V1"),
        }
    }
}

#[async_trait]
pub trait TransactionPreparator: Send + Sync + 'static {
    fn version(&self) -> PreparatorVersion;

    /// Returns [`VersionedMessage`] corresponding to [`ScheduledBaseIntent`] tasks
    /// Handles all necessary preparations for Message to be valid
    /// NOTE: [`VersionedMessage`] contains dummy recent_block_hash that should be replaced
    async fn prepare_commit_tx<P: IntentPersister>(
        &self,
        authority: &Keypair,
        base_intent: &ScheduledBaseIntent,
        intent_persister: &Option<P>,
    ) -> PreparatorResult<VersionedMessage>;

    /// Returns [`VersionedMessage`] corresponding to [`ScheduledBaseIntent`] tasks
    /// Handles all necessary preparations for Message to be valid
    // NOTE: [`VersionedMessage`] contains dummy recent_block_hash that should be replaced
    async fn prepare_finalize_tx<P: IntentPersister>(
        &self,
        authority: &Keypair,
        base_intent: &ScheduledBaseIntent,
        intent_persister: &Option<P>,
    ) -> PreparatorResult<VersionedMessage>;
}

/// [`TransactionPreparatorV1`] first version of preparator
/// It omits future commit_bundle/finalize_bundle logic
/// It creates TXs using current per account commit/finalize
pub struct TransactionPreparatorV1<C: CommitIdFetcher> {
    rpc_client: MagicblockRpcClient,
    commit_id_fetcher: Arc<C>,
    delivery_preparator: DeliveryPreparator,
    compute_budget_config: ComputeBudgetConfig,
}

impl<C> TransactionPreparatorV1<C>
where
    C: CommitIdFetcher,
{
    pub fn new(
        rpc_client: MagicblockRpcClient,
        table_mania: TableMania,
        compute_budget_config: ComputeBudgetConfig,
        commit_id_fetcher: Arc<C>,
    ) -> Self {
        let delivery_preparator = DeliveryPreparator::new(
            rpc_client.clone(),
            table_mania,
            compute_budget_config.clone(),
        );

        Self {
            rpc_client,
            commit_id_fetcher,
            delivery_preparator,
            compute_budget_config,
        }
    }
}

#[async_trait]
impl<C> TransactionPreparator for TransactionPreparatorV1<C>
where
    C: CommitIdFetcher,
{
    fn version(&self) -> PreparatorVersion {
        PreparatorVersion::V1
    }

    /// In V1: prepares TX with commits for every account in message
    /// For pure actions message - outputs Tx that runs actions
    async fn prepare_commit_tx<P: IntentPersister>(
        &self,
        authority: &Keypair,
        base_intent: &ScheduledBaseIntent,
        intent_persister: &Option<P>,
    ) -> PreparatorResult<VersionedMessage> {
        // create tasks
        let tasks = TaskBuilderV1::commit_tasks(
            &self.commit_id_fetcher,
            base_intent,
            intent_persister,
        )
        .await?;
        // optimize to fit tx size. aka Delivery Strategy
        let tx_strategy = TaskStrategist::build_strategy(
            tasks,
            &authority.pubkey(),
            intent_persister,
        )?;
        // Pre tx preparations. Create buffer accs + lookup tables
        let lookup_tables = self
            .delivery_preparator
            .prepare_for_delivery(
                authority,
                &tx_strategy,
                intent_persister,
            )
            .await?;

        // Build resulting TX to be executed
        let message = TransactionUtils::assemble_tasks_tx(
            authority,
            &tx_strategy.optimized_tasks,
            self.compute_budget_config.compute_unit_price,
            &lookup_tables,
        )
        .expect("TaskStrategist had to fail prior. This shouldn't be reachable")
        .message;

        Ok(message)
    }

    /// In V1: prepares single TX with finalize, undelegation + actions
    async fn prepare_finalize_tx<P: IntentPersister>(
        &self,
        authority: &Keypair,
        base_intent: &ScheduledBaseIntent,
        intent_presister: &Option<P>,
    ) -> PreparatorResult<VersionedMessage> {
        // create tasks
        let tasks =
            TaskBuilderV1::finalize_tasks(&self.rpc_client, base_intent).await?;
        // optimize to fit tx size. aka Delivery Strategy
        let tx_strategy = TaskStrategist::build_strategy(
            tasks,
            &authority.pubkey(),
            intent_presister,
        )?;
        // Pre tx preparations. Create buffer accs + lookup tables
        let lookup_tables = self
            .delivery_preparator
            .prepare_for_delivery(
                authority,
                &tx_strategy,
                intent_presister,
            )
            .await?;

        let message = TransactionUtils::assemble_tasks_tx(
            authority,
            &tx_strategy.optimized_tasks,
            self.compute_budget_config.compute_unit_price,
            &lookup_tables,
        )
        .expect("TaskStrategist had to fail prior. This shouldn't be reachable")
        .message;

        Ok(message)
    }
}
