use async_trait::async_trait;
use magicblock_program::{
    magic_scheduled_base_intent::ScheduledIntentBundle, outbox::ExecutionStage,
};
use solana_hash::Hash;
use solana_keypair::Address as Pubkey;
use solana_transaction::Transaction;

use crate::{
    intent_executor::{
        error::IntentExecutorResult, ExecutionOutput, IntentExecutionReport,
    },
    outbox::outbox_intent_bundles_reader::OutboxIntentBundlesReader,
};

pub mod outbox_client;
pub mod outbox_intent_bundles_reader;
pub(crate) mod utils;

#[async_trait]
pub trait OutboxClient: Send + Sync + 'static {
    type Error: std::error::Error + Send;
    /// Type that is able to read IntentBundles from Outbox
    /// Can be via AccountsDB, RpcClient or any other means
    type OutboxReader: OutboxIntentBundlesReader;

    /// Executes `Accept` tx and returns accepted intents
    async fn accept_scheduled_intents(
        &self,
    ) -> Result<
        Vec<ScheduledIntentBundle>,
        (Vec<ScheduledIntentBundle>, Self::Error),
    >;

    /// Sets execution stage for outbox intent
    /// Note: intent has to be accepted prior
    /// Calling with invalid state transitions will lead to `TransactionError`
    async fn set_intent_execution_stage(
        &self,
        intent_id: u64,
        stage: ExecutionStage,
    ) -> Result<(), Self::Error>;

    /// Processes intent results, submitting them on chain(ER)
    async fn notify_commit_sent(
        &self,
        meta: ScheduledBaseIntentMeta,
        result: &IntentExecutorResult<ExecutionOutput>,
        execution_report: &IntentExecutionReport,
    ) -> Result<(), Self::Error>;

    /// Returns reader capable of reading IntentBundles from Outbox
    fn outbox_reader(&self) -> Self::OutboxReader;
}

pub struct ScheduledBaseIntentMeta {
    pub id: u64,
    pub slot: u64,
    pub blockhash: Hash,
    pub payer: Pubkey,
    pub included_pubkeys: Vec<Pubkey>,
    pub intent_sent_transaction: Transaction,
    pub requested_undelegation: bool,
}

impl ScheduledBaseIntentMeta {
    pub(crate) fn new(intent: &ScheduledIntentBundle) -> Self {
        Self {
            id: intent.id,
            slot: intent.slot,
            blockhash: intent.blockhash,
            payer: intent.payer,
            included_pubkeys: intent.get_all_committed_pubkeys(),
            intent_sent_transaction: intent.sent_transaction.clone(),
            requested_undelegation: intent.has_undelegate_intent(),
        }
    }
}
