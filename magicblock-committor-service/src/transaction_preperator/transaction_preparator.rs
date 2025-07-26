use std::{collections::HashMap, fmt::Formatter};

use async_trait::async_trait;
use magicblock_program::magic_scheduled_l1_message::ScheduledL1Message;
use magicblock_rpc_client::MagicblockRpcClient;
use magicblock_table_mania::TableMania;
use solana_pubkey::Pubkey;
use solana_sdk::{
    message::VersionedMessage, signature::Keypair, signer::Signer,
};

use crate::{
    persist::L1MessagesPersisterIface,
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

    /// Returns [`VersionedMessage`] corresponding to [`ScheduledL1Message`] tasks
    /// Handles all necessary preparations for Message to be valid
    /// NOTE: [`VersionedMessage`] contains dummy recent_block_hash that should be replaced
    async fn prepare_commit_tx<P: L1MessagesPersisterIface>(
        &self,
        authority: &Keypair,
        l1_message: &ScheduledL1Message,
        commit_ids: &HashMap<Pubkey, u64>,
        l1_messages_persister: &Option<P>,
    ) -> PreparatorResult<VersionedMessage>;

    /// Returns [`VersionedMessage`] corresponding to [`ScheduledL1Message`] tasks
    /// Handles all necessary preparations for Message to be valid
    // NOTE: [`VersionedMessage`] contains dummy recent_block_hash that should be replaced
    async fn prepare_finalize_tx<P: L1MessagesPersisterIface>(
        &self,
        authority: &Keypair,
        rent_reimbursement: &Pubkey,
        l1_message: &ScheduledL1Message,
        l1_messages_persister: &Option<P>,
    ) -> PreparatorResult<VersionedMessage>;
}

/// [`TransactionPreparatorV1`] first version of preparator
/// It omits future commit_bundle/finalize_bundle logic
/// It creates TXs using current per account commit/finalize
pub struct TransactionPreparatorV1 {
    delivery_preparator: DeliveryPreparator,
    rpc_client: MagicblockRpcClient,
    table_mania: TableMania, // TODO(edwin): Arc<TableMania>?
}

impl TransactionPreparatorV1 {
    pub fn new(
        rpc_client: MagicblockRpcClient,
        table_mania: TableMania,
        compute_budget_config: ComputeBudgetConfig,
    ) -> Self {
        let delivery_preparator = DeliveryPreparator::new(
            rpc_client.clone(),
            table_mania.clone(),
            compute_budget_config,
        );
        Self {
            rpc_client,
            table_mania,
            delivery_preparator,
        }
    }
}

#[async_trait]
impl TransactionPreparator for TransactionPreparatorV1 {
    fn version(&self) -> PreparatorVersion {
        PreparatorVersion::V1
    }

    /// In V1: prepares TX with commits for every account in message
    /// For pure actions message - outputs Tx that runs actions
    async fn prepare_commit_tx<P: L1MessagesPersisterIface>(
        &self,
        authority: &Keypair,
        l1_message: &ScheduledL1Message,
        commit_ids: &HashMap<Pubkey, u64>,
        l1_messages_persister: &Option<P>,
    ) -> PreparatorResult<VersionedMessage> {
        // create tasks
        let tasks = TaskBuilderV1::commit_tasks(l1_message, commit_ids)?;
        // optimize to fit tx size. aka Delivery Strategy
        let tx_strategy = TaskStrategist::build_strategy(
            tasks,
            &authority.pubkey(),
            l1_messages_persister,
        )?;
        // Pre tx preparations. Create buffer accs + lookup tables
        let lookup_tables = self
            .delivery_preparator
            .prepare_for_delivery(
                authority,
                &tx_strategy,
                l1_messages_persister,
            )
            .await?;

        // Build resulting TX to be executed
        let message = TransactionUtils::assemble_tasks_tx(
            authority,
            &tx_strategy.optimized_tasks,
            &lookup_tables,
        )
        .message;
        Ok(message)
    }

    /// In V1: prepares single TX with finalize, undelegation + actions
    async fn prepare_finalize_tx<P: L1MessagesPersisterIface>(
        &self,
        authority: &Keypair,
        rent_reimbursement: &Pubkey,
        l1_message: &ScheduledL1Message,
        l1_messages_persister: &Option<P>,
    ) -> PreparatorResult<VersionedMessage> {
        // create tasks
        let tasks =
            TaskBuilderV1::finalize_tasks(l1_message, rent_reimbursement);
        // optimize to fit tx size. aka Delivery Strategy
        let tx_strategy = TaskStrategist::build_strategy(
            tasks,
            &authority.pubkey(),
            l1_messages_persister,
        )?;
        // Pre tx preparations. Create buffer accs + lookup tables
        let lookup_tables = self
            .delivery_preparator
            .prepare_for_delivery(
                authority,
                &tx_strategy,
                l1_messages_persister,
            )
            .await?;

        let message = TransactionUtils::assemble_tasks_tx(
            authority,
            &tx_strategy.optimized_tasks,
            &lookup_tables,
        )
        .message;
        Ok(message)
    }
}
