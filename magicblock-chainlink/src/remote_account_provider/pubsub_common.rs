use std::{collections::HashSet, fmt};

use dlp_api::state::{
    CommitRecord, DelegationMetadata, DelegationRecord, ProgramConfig,
};
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
        Self::from_url_with_limit(pubsub_url, commitment_config, None)
    }

    /// Like [Self::from_url] but with an explicit per-stream subscription
    /// limit that overrides the per-provider defaults when set.
    pub fn from_url_with_limit(
        pubsub_url: impl Into<String>,
        commitment_config: CommitmentConfig,
        limit_override: Option<usize>,
    ) -> Self {
        let pubsub_url = pubsub_url.into();
        let per_stream_subscription_limit =
            limit_override.or_else(|| Some(default_limit_for_url(&pubsub_url)));
        Self {
            pubsub_url,
            commitment_config,
            per_stream_subscription_limit,
        }
    }
}

/// Per-provider defaults for subscriptions per websocket connection, capped
/// so large subscription sets fan out across the connection pool instead of
/// serializing on one socket. Limits verified empirically (Aug 2026):
/// Helius rejects sub #1001 with -32006; Triton accepts 10k+ at ~400 subs/s;
/// QuickNode accepts 5k+ but subscribes slowly (~86/s), so a lower cap keeps
/// per-socket reconnect repair time bounded.
fn default_limit_for_url(pubsub_url: &str) -> usize {
    let url = pubsub_url.to_lowercase();
    if url.contains("helius") {
        HELIUS_PER_STREAM_SUBSCRIPTION_LIMIT
    } else if url.contains("quiknode") {
        QUICKNODE_PER_STREAM_SUBSCRIPTION_LIMIT
    } else {
        DEFAULT_PER_STREAM_SUBSCRIPTION_LIMIT
    }
}

/// Identifies the upstream subscription stream that produced a
/// [SubscriptionUpdate]. Account-subscription updates can be safely dropped
/// once their direct subscription is released, while program-subscription
/// updates must still be processed even for pubkeys that are no longer in the
/// account-subscription LRU (e.g. delegated accounts tracked only via their
/// owner program).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubscriptionSource {
    Account,
    Program,
    /// Provider-initiated replay of a subscription result that was consumed
    /// to resolve a fetch which subsequently failed. Must be processed
    /// regardless of the account's watch state.
    Replay,
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
    /// The upstream subscription stream that produced this update.
    pub source: SubscriptionSource,
}

impl SubscriptionUpdate {
    pub fn from_rpc_response(
        pubkey: Pubkey,
        rpc_response: RpcResponse<UiAccount>,
        source: SubscriptionSource,
    ) -> Self {
        let account: Option<Account> = rpc_response.value.to_account();
        Self {
            pubkey,
            slot: rpc_response.context.slot,
            account,
            source,
        }
    }
}

pub(crate) fn is_delegation_record_data(data: &[u8]) -> bool {
    data.len() >= DelegationRecord::size_with_discriminator()
        && DelegationRecord::try_from_bytes_with_discriminator(
            &data[..DelegationRecord::size_with_discriminator()],
        )
        .is_ok()
}

pub(crate) fn is_internal_dlp_account_data(data: &[u8]) -> bool {
    is_delegation_record_data(data)
        || DelegationMetadata::try_from_bytes_with_discriminator(data).is_ok()
        || CommitRecord::try_from_bytes_with_discriminator(data).is_ok()
        || ProgramConfig::try_from_bytes_with_discriminator(data).is_ok()
}

#[cfg(test)]
mod tests {
    use dlp_api::{
        args::{
            EncryptedBuffer, MaybeEncryptedInstruction, MaybeEncryptedIxData,
            PostDelegationActions,
        },
        state::DelegationRecord,
    };
    use solana_pubkey::Pubkey;

    use super::is_internal_dlp_account_data;

    #[test]
    fn delegation_record_with_post_delegation_actions_is_internal() {
        let deleg_record = DelegationRecord {
            authority: Pubkey::new_unique(),
            owner: Pubkey::new_unique(),
            delegation_slot: 1,
            lamports: 1_000,
            commit_frequency_ms: 2_000,
        };
        let mut data = vec![0; DelegationRecord::size_with_discriminator()];
        deleg_record.to_bytes_with_discriminator(&mut data).unwrap();
        let actions = PostDelegationActions {
            inserted_signers: 0,
            inserted_non_signers: 0,
            signers: vec![*Pubkey::new_unique().as_array()],
            non_signers: vec![],
            instructions: vec![MaybeEncryptedInstruction {
                program_id: 0,
                accounts: vec![],
                data: MaybeEncryptedIxData {
                    prefix: vec![1],
                    suffix: EncryptedBuffer::default(),
                },
            }],
        };
        data.extend_from_slice(&borsh::to_vec(&actions).unwrap());

        assert!(is_internal_dlp_account_data(&data));
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
    pub completion_token: CancellationToken,
}

#[derive(Debug)]
pub enum ChainPubsubActorMessage {
    AccountSubscribe {
        pubkey: Pubkey,
        retries: Option<usize>,
        response: oneshot::Sender<RemoteAccountProviderResult<()>>,
    },
    AccountSubscribeMultiple {
        pubkeys: HashSet<Pubkey>,
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

/// Helius enforces a hard cap of 1,000 subscriptions per websocket
/// connection (error -32006 above it); stay below with headroom.
pub const HELIUS_PER_STREAM_SUBSCRIPTION_LIMIT: usize = 900;
/// QuickNode accepts thousands of subs per connection but processes
/// subscribe calls slowly; keep per-socket reconnect repair time bounded.
pub const QUICKNODE_PER_STREAM_SUBSCRIPTION_LIMIT: usize = 1_500;
pub const DEFAULT_PER_STREAM_SUBSCRIPTION_LIMIT: usize = 2_000;

pub const SUBSCRIPTION_UPDATE_CHANNEL_SIZE: usize = 5_000;
pub const MESSAGE_CHANNEL_SIZE: usize = 1_000;

#[cfg(test)]
mod limit_tests {
    use super::*;

    fn limit_for(url: &str, limit_override: Option<usize>) -> Option<usize> {
        PubsubClientConfig::from_url_with_limit(
            url,
            CommitmentConfig::confirmed(),
            limit_override,
        )
        .per_stream_subscription_limit
    }

    #[test]
    fn per_provider_default_limits() {
        assert_eq!(
            limit_for("wss://mainnet.helius-rpc.com/?api-key=x", None),
            Some(HELIUS_PER_STREAM_SUBSCRIPTION_LIMIT)
        );
        assert_eq!(
            limit_for("wss://foo.solana-devnet.quiknode.pro/abc/", None),
            Some(QUICKNODE_PER_STREAM_SUBSCRIPTION_LIMIT)
        );
        assert_eq!(
            limit_for("wss://foo.mainnet.rpcpool.com/abc", None),
            Some(DEFAULT_PER_STREAM_SUBSCRIPTION_LIMIT)
        );
    }

    #[test]
    fn explicit_limit_overrides_provider_default() {
        assert_eq!(
            limit_for("wss://mainnet.helius-rpc.com/?api-key=x", Some(50)),
            Some(50)
        );
        assert_eq!(
            limit_for("wss://foo.mainnet.rpcpool.com/abc", Some(50)),
            Some(50)
        );
    }
}
