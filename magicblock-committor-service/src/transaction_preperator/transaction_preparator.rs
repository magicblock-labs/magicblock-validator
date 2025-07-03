use async_trait::async_trait;
use solana_rpc_client::rpc_client::RpcClient;
use solana_sdk::message::v0::Message;
use solana_sdk::transaction::Transaction;
use magicblock_program::magic_scheduled_l1_message::{
    ScheduledL1Message, MagicL1Message
};
use magicblock_rpc_client::MagicblockRpcClient;
use magicblock_table_mania::TableMania;
use crate::transaction_preperator::budget_calculator::{ComputeBudgetCalculator, ComputeBudgetCalculatorV1};
use crate::transaction_preperator::error::{Error, PreparatorResult};

/// Transaction Preparator version
/// Some actions maybe imnvalid per version
#[derive(Debug)]
pub enum PreparatorVersion {
    V1,
}


#[async_trait]
trait TransactionPreparator {
    type BudgetCalculator: ComputeBudgetCalculator;
    
    fn version(&self) -> PreparatorVersion;
    async fn prepare_commit_tx(&self, l1_message: &ScheduledL1Message) -> PreparatorResult<Message>;
    async fn prepare_finalize_tx(&self, l1_message: &ScheduledL1Message) -> PreparatorResult<Message>;

}

/// [`TransactionPreparatorV1`] first version of preparator
/// It omits future commit_bundle/finalize_bundle logic
/// It creates TXs using current per account commit/finalize
struct TransactionPreparatorV1 {
    rpc_client: MagicblockRpcClient,
    table_mania: TableMania // TODO(edwin): Arc<TableMania>?
}

impl TransactionPreparatorV1 {
    pub fn new(rpc_client: MagicblockRpcClient, table_mania: TableMania) -> Self {
        Self {
            rpc_client,
            table_mania
        }
    }
}

impl TransactionPreparator for TransactionPreparatorV1 {
    type BudgetCalculator = ComputeBudgetCalculatorV1;
    
    fn version(&self) -> PreparatorVersion {
        PreparatorVersion::V1
    }

    /// In V1: prepares TX with commits for every account in message
    /// For pure actions message - outputs Tx that runs actions
    async fn prepare_commit_tx(&self, l1_message: &ScheduledL1Message) -> PreparatorResult<Message> {
        todo!()
    }

    /// In V1: prepares single TX with finalize, undelegation + actions
    async fn prepare_finalize_tx(&self, l1_message: &ScheduledL1Message) -> PreparatorResult<Message> {
        if matches!(l1_message.l1_message, MagicL1Message::L1Actions(_)) {
            Err(Error::VersionError(PreparatorVersion::V1))
        } else {
            Ok(())
        }
    }
}