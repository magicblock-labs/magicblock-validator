use async_trait::async_trait;
use program::{
    SentCommit, magic_scheduled_base_intent::ScheduledIntentBundle, outbox::ExecutionStage,
    outbox_intent_bundles::OutboxIntentBundle,
};
use solana_hash::Hash;
use solana_keypair::Address as Pubkey;

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

    /// Submits the recorded settlement result and closes its outbox on the ER.
    async fn notify_commit_sent(
        &self,
        sent_commit: SentCommit,
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
            requested_undelegation: intent.has_undelegate_intent(),
        }
    }
}
