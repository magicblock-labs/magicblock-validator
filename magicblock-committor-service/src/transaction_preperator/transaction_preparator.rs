use async_trait::async_trait;
use magicblock_program::magic_scheduled_l1_message::{
    CommittedAccountV2, L1Action, MagicL1Message, ScheduledL1Message,
};
use magicblock_rpc_client::MagicblockRpcClient;
use magicblock_table_mania::TableMania;
use solana_sdk::message::v0::Message;

use crate::transaction_preperator::{
    budget_calculator::{ComputeBudgetCalculator, ComputeBudgetCalculatorV1},
    error::{Error, PreparatorResult},
    task_builder::{TaskBuilderV1, TasksBuilder},
    task_strategist::TaskStrategist,
};

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
        l1_message: &ScheduledL1Message,
    ) -> PreparatorResult<Message>;
    async fn prepare_finalize_tx(
        &self,
        l1_message: &ScheduledL1Message,
    ) -> PreparatorResult<Message>;
}

/// [`TransactionPreparatorV1`] first version of preparator
/// It omits future commit_bundle/finalize_bundle logic
/// It creates TXs using current per account commit/finalize
struct TransactionPreparatorV1 {
    rpc_client: MagicblockRpcClient,
    table_mania: TableMania, // TODO(edwin): Arc<TableMania>?
}

impl TransactionPreparatorV1 {
    pub fn new(
        rpc_client: MagicblockRpcClient,
        table_mania: TableMania,
    ) -> Self {
        Self {
            rpc_client,
            table_mania,
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

impl TransactionPreparator for TransactionPreparatorV1 {
    fn version(&self) -> PreparatorVersion {
        PreparatorVersion::V1
    }

    /// In V1: prepares TX with commits for every account in message
    /// For pure actions message - outputs Tx that runs actions
    async fn prepare_commit_tx(
        &self,
        l1_message: &ScheduledL1Message,
    ) -> PreparatorResult<Message> {
        // 1. create tasks
        // 2. optimize to fit tx size. aka Delivery Strategy
        // 3. Pre tx preparations. Create buffer accs + lookup tables
        // 4. Build resulting TX to be executed

        // 1.
        let tasks = TaskBuilderV1::commit_tasks(l1_message);
        // 2.
        let tx_strategy = TaskStrategist::build_strategy(tasks)?;
        // 3.

        todo!()
    }

    /// In V1: prepares single TX with finalize, undelegation + actions
    async fn prepare_finalize_tx(
        &self,
        l1_message: &ScheduledL1Message,
    ) -> PreparatorResult<Message> {
        let tasks = TaskBuilderV1::finalize_tasks(l1_message);
        let tx_strategy = TaskStrategist::build_strategy(tasks);

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
