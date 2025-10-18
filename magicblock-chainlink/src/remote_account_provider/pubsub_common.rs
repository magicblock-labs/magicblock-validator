use std::fmt;

use solana_account_decoder::UiAccount;
use solana_pubkey::Pubkey;
use solana_rpc_client_api::response::Response as RpcResponse;
use solana_sdk::commitment_config::CommitmentConfig;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use crate::remote_account_provider::RemoteAccountProviderResult;

#[derive(Debug, Clone)]
pub struct PubsubClientConfig {
    pub pubsub_url: String,
    pub commitment_config: CommitmentConfig,
}

impl PubsubClientConfig {
    pub fn from_url(
        pubsub_url: impl Into<String>,
        commitment_config: CommitmentConfig,
    ) -> Self {
        Self {
            pubsub_url: pubsub_url.into(),
            commitment_config,
        }
    }
}

#[derive(Debug, Clone)]
pub struct SubscriptionUpdate {
    pub pubkey: Pubkey,
    pub rpc_response: RpcResponse<UiAccount>,
}

impl fmt::Display for SubscriptionUpdate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "SubscriptionUpdate(pubkey: {}, update: {:?})",
            self.pubkey, self.rpc_response
        )
    }
}

pub struct AccountSubscription {
    pub cancellation_token: CancellationToken,
}

#[derive(Debug)]
pub enum ChainPubsubActorMessage {
    AccountSubscribe {
        pubkey: Pubkey,
        response: oneshot::Sender<RemoteAccountProviderResult<()>>,
    },
    AccountUnsubscribe {
        pubkey: Pubkey,
        response: oneshot::Sender<RemoteAccountProviderResult<()>>,
    },
    RecycleConnections {
        response: oneshot::Sender<RemoteAccountProviderResult<()>>,
    },
}

pub const SUBSCRIPTION_UPDATE_CHANNEL_SIZE: usize = 5_000;
pub const MESSAGE_CHANNEL_SIZE: usize = 1_000;
