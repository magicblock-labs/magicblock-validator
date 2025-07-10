use std::collections::HashMap;

use crate::transaction_preperator::delivery_preparator::DeliveryPreparator;
use crate::transaction_preperator::{
    budget_calculator::{ComputeBudgetCalculator, ComputeBudgetCalculatorV1},
    error::{Error, PreparatorResult},
    task_builder::{TaskBuilderV1, TasksBuilder},
    task_strategist::TaskStrategist,
};
use crate::ComputeBudgetConfig;
use async_trait::async_trait;
use magicblock_program::magic_scheduled_l1_message::{
    CommittedAccountV2, L1Action, MagicL1Message, ScheduledL1Message,
};
use magicblock_rpc_client::MagicblockRpcClient;
use magicblock_table_mania::TableMania;
use solana_pubkey::Pubkey;
use solana_sdk::{message::v0::Message, signature::Keypair, signer::Signer};

/// Transaction Preparator version
/// Some actions maybe imnvalid per version
#[derive(Debug)]
pub enum PreparatorVersion {
    V1,
}

#[async_trait]
trait TransactionPreparator {
    fn version(&self) -> PreparatorVersion;
    async fn prepare_commit_tx(
        &self,
        authority: &Keypair,
        l1_message: &ScheduledL1Message,
        commit_ids: HashMap<Pubkey, u64>,
    ) -> PreparatorResult<Message>;
    async fn prepare_finalize_tx(
        &self,
        authority: &Keypair,
        rent_reimbursement: &Pubkey,
        l1_message: &ScheduledL1Message,
    ) -> PreparatorResult<Message>;
}

/// [`TransactionPreparatorV1`] first version of preparator
/// It omits future commit_bundle/finalize_bundle logic
/// It creates TXs using current per account commit/finalize
struct TransactionPreparatorV1 {
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

    // TODO(edwin)
    fn prepare_action_tx(actions: &Vec<L1Action>) -> PreparatorResult<Message> {
        todo!()
    }

    fn prepare_committed_accounts_tx(
        account: &Vec<CommittedAccountV2>,
    ) -> PreparatorResult<Message> {
        todo!()
    }
}

#[async_trait]
impl TransactionPreparator for TransactionPreparatorV1 {
    fn version(&self) -> PreparatorVersion {
        PreparatorVersion::V1
    }

    /// In V1: prepares TX with commits for every account in message
    /// For pure actions message - outputs Tx that runs actions
    async fn prepare_commit_tx(
        &self,
        authority: &Keypair,
        l1_message: &ScheduledL1Message,
        commit_ids: HashMap<Pubkey, u64>,
    ) -> PreparatorResult<Message> {
        // 1. create tasks
        // 2. optimize to fit tx size. aka Delivery Strategy
        // 3. Pre tx preparations. Create buffer accs + lookup tables
        // 4. Build resulting TX to be executed

        // 1.
        let tasks = TaskBuilderV1::commit_tasks(l1_message, commit_ids);
        // 2.
        let tx_strategy =
            TaskStrategist::build_strategy(tasks, &authority.pubkey())?;
        // 3.
        let _ = self
            .delivery_preparator
            .prepare_for_delivery(authority, &tx_strategy)
            .await
            .unwrap(); // TODO: fix

        todo!()
    }

    /// In V1: prepares single TX with finalize, undelegation + actions
    async fn prepare_finalize_tx(
        &self,
        authority: &Keypair,
        rent_reimbursement: &Pubkey,
        l1_message: &ScheduledL1Message,
    ) -> PreparatorResult<Message> {
        let tasks =
            TaskBuilderV1::finalize_tasks(l1_message, rent_reimbursement);
        let tx_strategy =
            TaskStrategist::build_strategy(tasks, &authority.pubkey())?;
        let _ = self
            .delivery_preparator
            .prepare_for_delivery(authority, &tx_strategy)
            .await
            .unwrap(); // TODO: fix

        todo!()
    }
}

/// We have 2 stages for L1Message
/// 1. commit
/// 2. finalize
///
/// Now, single "task" can be differently represented in 2 stage
/// In terms of transaction and so on

/// We have:
/// Stages - type
/// Strategy - enum
/// Task - enum

// Can [`Task`] have [`Strategy`] based on [`Stage`]
// We receive proposals and actions from users
// Those have to

/// We get tasks we need to pass them through
/// Strategy:
// 1. Try to fit Vec<T: Serialize> into TX. save tx_size
// 2. Start optimizing
// 3. Find biggest ix
// 4. Replace with BufferedIx(maybe pop from Heap)
// 5. tx_size -= (og_size - buffered_size)
// 6. If doesn't fit - continue
// 7. If heap.is_empty() - doesn't fit with buffered
// 8. Apply lookup table
// 9. if fits - return Ok(tx), else return Err(Failed)

// Committor flow:
// 1. Gets commits
// 2. Passes to Scheduler
// 3. Scheduler checks if any can run in parallel. Does scheduling basically
// 4. Calls TransactionPreparator for those
// 5. Executes TXs if all ok
// 6. Populates Persister with necessary data

fn useless() {}
