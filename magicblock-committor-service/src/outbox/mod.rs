use magicblock_program::magic_scheduled_base_intent::ScheduledIntentBundle;
use solana_hash::Hash;
use solana_keypair::Address as Pubkey;
use solana_transaction::Transaction;

pub mod outbox_client;
pub mod outbox_intent_bundles_reader;
pub(crate) mod utils;

pub struct ScheduledBaseIntentMeta {
    pub(crate) id: u64,
    pub(crate) slot: u64,
    pub(crate) blockhash: Hash,
    pub(crate) payer: Pubkey,
    pub(crate) included_pubkeys: Vec<Pubkey>,
    pub(crate) intent_sent_transaction: Transaction,
    pub(crate) requested_undelegation: bool,
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
