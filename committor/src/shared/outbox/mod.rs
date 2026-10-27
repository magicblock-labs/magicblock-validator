use async_trait::async_trait;
use program::{
    SentCommit, magic_scheduled_base_intent::ScheduledIntentBundle, outbox::ExecutionStage,
    outbox_intent_bundles::OutboxIntentBundle,
};
use solana_hash::Hash;
use solana_keypair::Address as Pubkey;
use solana_transaction::Transaction;

use outbox_client::InternalOutboxClientError;

pub(crate) mod outbox_client;
pub(crate) mod outbox_intent_bundles_reader;
pub(crate) mod progress;

#[async_trait]
pub(crate) trait OutboxClient: Send + Sync + 'static {
    /// Sets execution stage for outbox intent
    /// Note: intent has to be accepted prior
    /// Calling with invalid state transitions will lead to `TransactionError`
    async fn set_intent_execution_stage(
        &self,
        intent_id: u64,
        stage: ExecutionStage,
        recovery_commit_nonces: Vec<(Pubkey, u64)>,
    ) -> Result<(), InternalOutboxClientError>;

    /// Registers the result payload and submits its notification on the ER.
    ///
    /// Fresh intents preserve their scheduling-time signature. Recovered
    /// intents, and fresh intents whose blockhash expired before execution,
    /// are signed against the engine's current blockhash.
    async fn notify_commit_sent(
        &self,
        sent_commit: SentCommit,
        transaction: IntentSentTransaction,
    ) -> Result<(), InternalOutboxClientError>;

    async fn fetch_outbox_intent(
        &self,
        intent_id: u64,
    ) -> Result<Option<OutboxIntentBundle>, InternalOutboxClientError>;
}

pub(crate) struct ScheduledBaseIntentMeta {
    pub id: u64,
    pub slot: u64,
    pub blockhash: Hash,
    pub payer: Pubkey,
    pub included_pubkeys: Vec<Pubkey>,
    pub intent_sent_transaction: IntentSentTransaction,
    pub requested_undelegation: bool,
}

impl ScheduledBaseIntentMeta {
    pub(crate) fn new(intent: &ScheduledIntentBundle) -> Self {
        Self {
            id: intent.intent_id,
            slot: intent.slot,
            blockhash: intent.blockhash,
            payer: intent.payer,
            included_pubkeys: intent.get_all_committed_pubkeys(),
            intent_sent_transaction: if intent.sent_transaction.signatures.is_empty() {
                IntentSentTransaction::Recovered
            } else {
                IntentSentTransaction::Known(intent.sent_transaction.clone())
            },
            requested_undelegation: intent.has_undelegate_intent(),
        }
    }
}

/// Tracks the `ScheduledCommitSent` transaction for an in-flight intent.
///
/// Bundles recovered from the on-chain outbox have a stale `sent_transaction`
/// (original blockhash is expired by restart time). `Recovered` signals that
/// the transaction must be rebuilt at notification time with a fresh ER blockhash.
#[derive(Default)]
pub(crate) enum IntentSentTransaction {
    Known(Transaction),
    #[default]
    Recovered,
}
