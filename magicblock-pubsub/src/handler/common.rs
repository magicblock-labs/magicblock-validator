use jsonrpc_pubsub::Sink;
use magicblock_geyser_plugin::types::SubscriptionsDb;
use serde::{Deserialize, Serialize};
use solana_account_decoder::UiAccount;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct UiAccountWithPubkey {
    pub pubkey: String,
    pub account: UiAccount,
}

pub struct UpdateHandler<B, C> {
    pub sink: Sink,
    pub builder: B,
    pub subscriptions_db: SubscriptionsDb,
    pub cleanup: C,
}
