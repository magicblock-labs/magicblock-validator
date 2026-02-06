use std::fmt;

use solana_account::Account;
use solana_account_decoder::UiAccount;
use solana_clock::Slot;
use solana_commitment_config::CommitmentConfig;
use solana_pubkey::Pubkey;
use solana_rpc_client_api::response::Response as RpcResponse;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use crate::remote_account_provider::RemoteAccountProviderResult;

#[derive(Debug, Clone)]
pub struct PubsubClientConfig {
    pub pubsub_url: String,
    pub commitment_config: CommitmentConfig,
    pub per_stream_subscription_limit: Option<usize>,
}

impl PubsubClientConfig {
    pub fn from_url(
        pubsub_url: impl Into<String>,
        commitment_config: CommitmentConfig,
    ) -> Self {
        let pubsub_url = pubsub_url.into();
        let per_stream_subscription_limit =
            if pubsub_url.to_lowercase().contains("helius") {
                Some(HELIUS_PER_STREAM_SUBSCRIPTION_LIMIT)
            } else {
                None
            };
        Self {
            pubsub_url,
            commitment_config,
            per_stream_subscription_limit,
        }
    }
}

#[derive(Debug, Clone)]
pub struct SubscriptionUpdate {
    /// The pubkey of the account that was updated
    pub pubkey: Pubkey,
    /// The remote slot at which the update occurred
    pub slot: Slot,
    /// The updated account.
    /// It is `None` if the [UiAccount] of an [RpcResponse] could not be decoded
    pub account: Option<Account>,
}

impl From<(Pubkey, RpcResponse<UiAccount>)> for SubscriptionUpdate {
    fn from((pubkey, rpc_response): (Pubkey, RpcResponse<UiAccount>)) -> Self {
        let account: Option<Account> = rpc_response.value.decode::<Account>();
        Self {
            pubkey,
            slot: rpc_response.context.slot,
            account,
        }
    }
}

impl fmt::Display for SubscriptionUpdate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "SubscriptionUpdate(pubkey: {}, update: {:?}) at slot {}",
            self.pubkey, self.account, self.slot
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
        retries: Option<usize>,
        response: oneshot::Sender<RemoteAccountProviderResult<()>>,
    },
    AccountUnsubscribe {
        pubkey: Pubkey,
        response: oneshot::Sender<RemoteAccountProviderResult<()>>,
    },
    ProgramSubscribe {
        pubkey: Pubkey,
        response: oneshot::Sender<RemoteAccountProviderResult<()>>,
    },
    Reconnect {
        response: oneshot::Sender<RemoteAccountProviderResult<()>>,
    },
    Shutdown {
        response: oneshot::Sender<RemoteAccountProviderResult<()>>,
    },
}

pub const HELIUS_PER_STREAM_SUBSCRIPTION_LIMIT: usize = 100;

pub const SUBSCRIPTION_UPDATE_CHANNEL_SIZE: usize = 5_000;
pub const MESSAGE_CHANNEL_SIZE: usize = 1_000;