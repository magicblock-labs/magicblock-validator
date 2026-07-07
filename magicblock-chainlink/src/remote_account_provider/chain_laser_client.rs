use std::{
    collections::HashSet,
    sync::{Arc, Mutex},
    time::Duration,
};

use async_trait::async_trait;
use magicblock_config::config::GrpcConfig;
use solana_commitment_config::CommitmentLevel;
use solana_pubkey::{pubkey, Pubkey};
use solana_sdk_ids::sysvar::clock;
use tokio::sync::{mpsc, oneshot};
use tracing::*;

use crate::remote_account_provider::{
    chain_laser_actor::{ChainLaserActor, SharedSubscriptions, Slots},
    chain_rpc_client::ChainRpcClientImpl,
    pubsub_common::{ChainPubsubActorMessage, SubscriptionUpdate},
    ChainPubsubClient, ReconnectableClient, RemoteAccountProviderError,
    RemoteAccountProviderResult,
};

/// Reserved pubkey used to track implicit slot subscriptions for GRPC clients.
///
/// ## Design Rationale
///
/// GRPC clients receive slot updates directly via the SubscribeRequestFilterSlots
/// filter in their subscription request, but don't subscribe to the clock::ID account.
/// Instead of skipping subscription tracking or implementing special-case logic, we
/// subscribe to this dummy pubkey to represent the implicit slot subscription.
///
/// ## Benefits
///
/// **Accuracy**: Subscription count equals the actual number of active subscriptions
/// across all client types (WebSocket and GRPC). No hidden logic, no off-by-one errors.
///
/// **No Hidden Assumptions**: The dummy subscription is tracked like any other
/// subscription in our data structures. Metrics, logging, and subscription management
/// code can treat all subscriptions uniformly without special cases or exceptions.
/// This prevents subtle bugs where GRPC and WebSocket clients are handled differently.
///
/// The value used is a reserved, non-functional pubkey that never carries real account
/// data. It exists purely for accounting purposes.
static SLOT_SUBSCRIPTION_DUMMY: Pubkey =
    pubkey!("FAKESUB111111111111111111111111111111111111");

/// Upper bound on any round-trip to the actor. Normally these complete in
/// milliseconds; if the actor is wedged (e.g. on a dead gRPC connection) this
/// converts an indefinite hang of every caller into an error, which the
/// fetch/subscription layers handle and retry.
const ACTOR_RESPONSE_TIMEOUT: Duration = Duration::from_secs(30);

/// Awaits the actor's oneshot response, bounded by
/// [`ACTOR_RESPONSE_TIMEOUT`].
async fn await_actor_response<T>(
    rx: oneshot::Receiver<RemoteAccountProviderResult<T>>,
    op: &str,
) -> RemoteAccountProviderResult<T> {
    match tokio::time::timeout(ACTOR_RESPONSE_TIMEOUT, rx).await {
        Ok(response) => response?,
        Err(_) => Err(RemoteAccountProviderError::ChainPubsubActorTimeout(
            op.to_string(),
        )),
    }
}

#[derive(Clone)]
pub struct ChainLaserClientImpl {
    /// Receiver for subscription updates
    updates: Arc<Mutex<Option<mpsc::Receiver<SubscriptionUpdate>>>>,
    /// Channel to send messages to the actor
    messages: mpsc::Sender<ChainPubsubActorMessage>,
    /// Shared subscriptions with the actor for sync access
    subscriptions: SharedSubscriptions,
    /// Client identifier
    client_id: String,
}

impl ChainLaserClientImpl {
    #[allow(clippy::too_many_arguments)]
    pub fn new_from_url(
        pubsub_url: &str,
        client_id: String,
        api_key: &str,
        commitment: CommitmentLevel,
        abort_sender: mpsc::Sender<()>,
        slots: Slots,
        rpc_client: ChainRpcClientImpl,
        grpc_config: &GrpcConfig,
    ) -> Self {
        let (actor, messages, updates, subscriptions) =
            ChainLaserActor::new_from_url(
                pubsub_url,
                &client_id,
                api_key,
                commitment,
                abort_sender,
                slots,
                rpc_client,
                grpc_config,
            );
        let client = Self {
            updates: Arc::new(Mutex::new(Some(updates))),
            messages,
            subscriptions,
            client_id,
        };
        tokio::spawn(actor.run());
        client
    }

    async fn subscribe_multiple(
        &self,
        pubkeys: HashSet<Pubkey>,
        retries: Option<usize>,
    ) -> RemoteAccountProviderResult<()> {
        // Map clock::ID to SLOT_SUBSCRIPTION_DUMMY, matching the
        // single-subscribe path so GRPC clients never create a real
        // clock account subscription.
        let pubkeys: HashSet<Pubkey> = pubkeys
            .into_iter()
            .map(|pk| {
                if pk == clock::ID {
                    SLOT_SUBSCRIPTION_DUMMY
                } else {
                    pk
                }
            })
            .collect();

        let (tx, rx) = oneshot::channel();
        self.send_msg(ChainPubsubActorMessage::AccountSubscribeMultiple {
            pubkeys,
            retries,
            response: tx,
        })
        .await?;

        await_actor_response(rx, "subscribe_multiple").await
    }

    #[instrument(skip(self, msg), fields(client_id = %self.client_id))]
    async fn send_msg(
        &self,
        msg: ChainPubsubActorMessage,
    ) -> RemoteAccountProviderResult<()> {
        // The message channel only backs up when the actor stopped draining
        // it; bound the send so callers fail instead of hanging with it.
        match tokio::time::timeout(
            ACTOR_RESPONSE_TIMEOUT,
            self.messages.send(msg),
        )
        .await
        {
            Ok(sent) => sent.map_err(|err| {
                RemoteAccountProviderError::ChainLaserActorSendError(
                    err.to_string(),
                    format!("{err:#?}"),
                )
            }),
            Err(_) => Err(RemoteAccountProviderError::ChainPubsubActorTimeout(
                "send_msg".to_string(),
            )),
        }
    }
}

#[async_trait]
impl ChainPubsubClient for ChainLaserClientImpl {
    async fn subscribe(
        &self,
        pubkey: Pubkey,
        retries: Option<usize>,
    ) -> RemoteAccountProviderResult<()> {
        // Skip clock::ID subscriptions for GRPC clients since they get slot
        // updates directly via the SubscribeRequestFilterSlots in the GRPC
        // subscription request. Instead, subscribe to a dummy pubkey to track
        // this implicit subscription and keep subscription counts accurate.
        // Subscription counts equal for different client types by treating the GRPC slot sub the
        // same as the websocket clock sub.
        // Otherwise we'd have to handle the inconsistency of account subscription counts of websocket vs
        // GRPC clients in multiple places.
        let effective_pubkey = if pubkey == clock::ID {
            SLOT_SUBSCRIPTION_DUMMY
        } else {
            pubkey
        };

        let (tx, rx) = oneshot::channel();
        self.send_msg(ChainPubsubActorMessage::AccountSubscribe {
            pubkey: effective_pubkey,
            retries,
            response: tx,
        })
        .await?;

        await_actor_response(rx, "subscribe").await
    }

    async fn subscribe_program(
        &self,
        program_id: Pubkey,
    ) -> RemoteAccountProviderResult<()> {
        let (tx, rx) = oneshot::channel();
        self.send_msg(ChainPubsubActorMessage::ProgramSubscribe {
            pubkey: program_id,
            response: tx,
        })
        .await?;

        await_actor_response(rx, "subscribe_program").await
    }

    async fn unsubscribe(
        &self,
        pubkey: Pubkey,
    ) -> RemoteAccountProviderResult<()> {
        let (tx, rx) = oneshot::channel();
        self.send_msg(ChainPubsubActorMessage::AccountUnsubscribe {
            pubkey,
            response: tx,
        })
        .await?;

        await_actor_response(rx, "unsubscribe").await
    }

    async fn shutdown(&self) -> RemoteAccountProviderResult<()> {
        let (tx, rx) = oneshot::channel();
        self.send_msg(ChainPubsubActorMessage::Shutdown { response: tx })
            .await?;

        await_actor_response(rx, "shutdown").await
    }

    fn take_updates(&self) -> mpsc::Receiver<SubscriptionUpdate> {
        let mut updates_lock = self.updates.lock().unwrap();
        updates_lock
            .take()
            .expect("ChainLaserClientImpl::take_updates called more than once")
    }

    fn subscriptions_union(&self) -> HashSet<Pubkey> {
        self.subscriptions.read().clone()
    }

    fn subs_immediately(&self) -> bool {
        // All gRPC clients backfill, so delayed subscriptions behave like
        // immediate subscriptions from the caller's perspective.
        true
    }

    fn id(&self) -> &str {
        &self.client_id
    }
}

#[async_trait]
impl ReconnectableClient for ChainLaserClientImpl {
    async fn try_reconnect(&self) -> RemoteAccountProviderResult<()> {
        let (tx, rx) = oneshot::channel();
        self.send_msg(ChainPubsubActorMessage::Reconnect { response: tx })
            .await?;

        await_actor_response(rx, "reconnect")
            .await
            .inspect_err(|err| {
                warn!(error = ?err, "Error while awaiting reconnect response");
            })
    }

    async fn resub_multiple(
        &self,
        pubkeys: HashSet<Pubkey>,
    ) -> RemoteAccountProviderResult<()> {
        self.subscribe_multiple(pubkeys, None).await?;
        Ok(())
    }
}
