use std::{
    collections::{HashMap, HashSet},
    fmt,
    sync::{Arc, Mutex},
};

use log::*;
use solana_account_decoder_client_types::{UiAccount, UiAccountEncoding};
use solana_pubkey::Pubkey;
use solana_rpc_client_api::{
    config::RpcAccountInfoConfig, response::Response as RpcResponse,
};
use solana_sdk::{commitment_config::CommitmentConfig, sysvar::clock};
use tokio::sync::{mpsc, oneshot, Mutex as AsyncMutex};
use tokio_stream::StreamExt;
use tokio_util::sync::CancellationToken;

use super::{
    chain_pubsub_client::PubSubConnection,
    errors::{RemoteAccountProviderError, RemoteAccountProviderResult},
};

// Log every 10 secs (given chain slot time is 400ms)
const CLOCK_LOG_SLOT_FREQ: u64 = 25;
const MAX_SUBSCRIBE_ATTEMPTS: usize = 3;

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

struct AccountSubscription {
    cancellation_token: CancellationToken,
}

// -----------------
// ChainPubsubActor
// -----------------
pub struct ChainPubsubActor {
    /// Configuration used to create the pubsub client
    pubsub_client_config: PubsubClientConfig,
    /// Underlying pubsub connection to connect to the chain
    pubsub_connection: Arc<PubSubConnection>,
    /// Sends subscribe/unsubscribe messages to this actor
    messages_sender: mpsc::Sender<ChainPubsubActorMessage>,
    /// Map of subscriptions we are holding
    subscriptions: Arc<Mutex<HashMap<Pubkey, AccountSubscription>>>,
    /// Sends updates for any account subscription that is received via
    /// the [Self::pubsub_connection]
    subscription_updates_sender: mpsc::Sender<SubscriptionUpdate>,
    /// Lock to prevent concurrent recycle attempts
    recycle_lock: Arc<AsyncMutex<()>>,
    /// The token to use to cancel all subscriptions and shut down the
    /// message listener, essentially shutting down whis actor
    shutdown_token: CancellationToken,
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
}

const SUBSCRIPTION_UPDATE_CHANNEL_SIZE: usize = 5_000;
const MESSAGE_CHANNEL_SIZE: usize = 1_000;

impl ChainPubsubActor {
    pub async fn new_from_url(
        pubsub_url: &str,
        commitment: CommitmentConfig,
    ) -> RemoteAccountProviderResult<(Self, mpsc::Receiver<SubscriptionUpdate>)>
    {
        let config = PubsubClientConfig::from_url(pubsub_url, commitment);
        Self::new(config).await
    }

    pub async fn new(
        pubsub_client_config: PubsubClientConfig,
    ) -> RemoteAccountProviderResult<(Self, mpsc::Receiver<SubscriptionUpdate>)>
    {
        let url = pubsub_client_config.pubsub_url.clone();
        let pubsub_connection = Arc::new(PubSubConnection::new(url).await?);

        let (subscription_updates_sender, subscription_updates_receiver) =
            mpsc::channel(SUBSCRIPTION_UPDATE_CHANNEL_SIZE);
        let (messages_sender, messages_receiver) =
            mpsc::channel(MESSAGE_CHANNEL_SIZE);

        let shutdown_token = CancellationToken::new();
        let recycle_lock = Arc::new(AsyncMutex::new(()));
        let me = Self {
            pubsub_client_config,
            pubsub_connection,
            messages_sender,
            subscriptions: Default::default(),
            subscription_updates_sender,
            recycle_lock,
            shutdown_token,
        };
        me.start_worker(messages_receiver);

        // Listened on by the client of this actor to receive updates for
        // subscribed accounts
        Ok((me, subscription_updates_receiver))
    }

    pub async fn shutdown(&self) {
        info!("Shutting down ChainPubsubActor");
        let subs = self
            .subscriptions
            .lock()
            .unwrap()
            .drain()
            .collect::<Vec<_>>();
        for (_, sub) in subs {
            sub.cancellation_token.cancel();
        }
        self.shutdown_token.cancel();
    }

    pub fn subscription_count(&self, filter: &[Pubkey]) -> usize {
        let subs = self
            .subscriptions
            .lock()
            .expect("subscriptions lock poisoned");
        if filter.is_empty() {
            subs.len()
        } else {
            subs.keys()
                .filter(|pubkey| !filter.contains(pubkey))
                .count()
        }
    }

    pub fn subscriptions(&self) -> Vec<Pubkey> {
        let subs = self
            .subscriptions
            .lock()
            .expect("subscriptions lock poisoned");
        subs.keys().copied().collect()
    }

    pub async fn send_msg(
        &self,
        msg: ChainPubsubActorMessage,
    ) -> RemoteAccountProviderResult<()> {
        self.messages_sender.send(msg).await.map_err(|err| {
            RemoteAccountProviderError::ChainPubsubActorSendError(
                err.to_string(),
                format!("{err:#?}"),
            )
        })
    }

    fn start_worker(
        &self,
        mut messages_receiver: mpsc::Receiver<ChainPubsubActorMessage>,
    ) {
        let subs = self.subscriptions.clone();
        let shutdown_token = self.shutdown_token.clone();
        let pubsub_client_config = self.pubsub_client_config.clone();
        let subscription_updates_sender =
            self.subscription_updates_sender.clone();
        let pubsub_connection = self.pubsub_connection.clone();
        let recycle_lock = self.recycle_lock.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    msg = messages_receiver.recv() => {
                        if let Some(msg) = msg {
                            Self::handle_msg(
                                subs.clone(),
                                pubsub_connection.clone(),
                                subscription_updates_sender.clone(),
                                pubsub_client_config.clone(),
                                recycle_lock.clone(),
                                msg
                            ).await;
                        } else {
                            break;
                        }
                    }
                    _ = shutdown_token.cancelled() => {
                        break;
                    }
                }
            }
        });
    }

    async fn handle_msg(
        subscriptions: Arc<Mutex<HashMap<Pubkey, AccountSubscription>>>,
        pubsub_connection: Arc<PubSubConnection>,
        subscription_updates_sender: mpsc::Sender<SubscriptionUpdate>,
        pubsub_client_config: PubsubClientConfig,
        recycle_lock: Arc<AsyncMutex<()>>,
        msg: ChainPubsubActorMessage,
    ) {
        match msg {
            ChainPubsubActorMessage::AccountSubscribe { pubkey, response } => {
                let commitment_config = pubsub_client_config.commitment_config;
                Self::add_sub(
                    pubkey,
                    response,
                    subscriptions,
                    pubsub_connection,
                    subscription_updates_sender,
                    commitment_config,
                    recycle_lock,
                );
            }
            ChainPubsubActorMessage::AccountUnsubscribe {
                pubkey,
                response,
            } => {
                if let Some(AccountSubscription { cancellation_token }) =
                    subscriptions
                        .lock()
                        .expect("subcriptions lock poisoned")
                        .get(&pubkey)
                {
                    cancellation_token.cancel();
                    let _ = response.send(Ok(()));
                } else {
                    let _ = response
                        .send(Err(RemoteAccountProviderError::AccountSubscriptionDoesNotExist(
                            pubkey.to_string(),
                        )));
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn add_sub(
        pubkey: Pubkey,
        sub_response: oneshot::Sender<RemoteAccountProviderResult<()>>,
        subs: Arc<Mutex<HashMap<Pubkey, AccountSubscription>>>,
        pubsub_connection: Arc<PubSubConnection>,
        subscription_updates_sender: mpsc::Sender<SubscriptionUpdate>,
        commitment_config: CommitmentConfig,
        recycle_lock: Arc<AsyncMutex<()>>,
    ) {
        if subs
            .lock()
            .expect("subscriptions lock poisoned")
            .contains_key(&pubkey)
        {
            trace!("Subscription for {pubkey} already exists, ignoring add_sub request");
            let _ = sub_response.send(Ok(()));
            return;
        }

        trace!("Adding subscription for {pubkey} with commitment {commitment_config:?}");

        let cancellation_token = CancellationToken::new();

        // Insert into subscriptions HashMap immediately to prevent race condition
        // with unsubscribe operations
        // Assuming that messages to this actor are processed in the order they are sent
        // then this eliminates the possibility of an unsubscribe being processed before
        // the sub's cancellation token was added to the map
        {
            let mut subs_lock =
                subs.lock().expect("subscriptions lock poisoned");
            subs_lock.insert(
                pubkey,
                AccountSubscription {
                    cancellation_token: cancellation_token.clone(),
                },
            );
        }

        tokio::spawn(async move {
            let config = RpcAccountInfoConfig {
                commitment: Some(commitment_config),
                encoding: Some(UiAccountEncoding::Base64Zstd),
                ..Default::default()
            };
            // Attempt to subscribe to the account
            let mut attempts = 1;
            let (mut update_stream, unsubscribe) = loop {
                let res = pubsub_connection
                    .account_subscribe(&pubkey, config.clone());
                match res.await {
                    Ok(res) => break res,
                    Err(err) => {
                        if attempts == MAX_SUBSCRIBE_ATTEMPTS {
                            // At this point we just give up and report to caller
                            subs.lock()
                                .expect("subscriptions lock poisoned")
                                .remove(&pubkey);
                            let _ = sub_response.send(Err(err.into()));
                            return;
                        }
                        attempts += 1;
                    }
                }
                // When the subscription attempt failed but we did not yet run out of retries,
                // attempt to recreate the connection with all of its subscriptions in the background.
                let pubsub_connection_clone = pubsub_connection.clone();
                let subs_clone = subs.clone();
                let subscription_updates_sender_clone =
                    subscription_updates_sender.clone();
                let recycle_lock_clone = recycle_lock.clone();
                if let Err(err) = Self::recycle_connection(
                    pubsub_connection_clone,
                    subs_clone,
                    subscription_updates_sender_clone,
                    commitment_config,
                    recycle_lock_clone,
                    Some(pubkey),
                )
                .await
                {
                    error!(
                        "RecycleConnections: supervisor task failed: {err:?}"
                    );
                }
            };

            // RPC succeeded - confirm to the requester that the subscription was made
            let _ = sub_response.send(Ok(()));

            // Now keep listening for updates and relay them to the
            // subscription updates sender until it is cancelled
            loop {
                tokio::select! {
                    _ = cancellation_token.cancelled() => {
                        trace!("Subscription for {pubkey} was cancelled");
                        break;
                    }
                    update = update_stream.next() => {
                        if let Some(rpc_response) = update {
                            if log_enabled!(log::Level::Trace) && (!pubkey.eq(&clock::ID) ||
                               rpc_response.context.slot % CLOCK_LOG_SLOT_FREQ == 0) {
                                trace!("Received update for {pubkey}: {rpc_response:?}");
                            }
                            let _ = subscription_updates_sender.send(SubscriptionUpdate {
                                pubkey,
                                rpc_response,
                            }).await.inspect_err(|err| {
                                error!("Failed to send {pubkey} subscription update: {err:?}");
                            });
                        } else {
                            trace!("Subscription for {pubkey} ended by update stream");
                            break;
                        }
                    }
                }
            }

            // Clean up subscription
            unsubscribe().await;
            subs.lock()
                .expect("subscriptions lock poisoned")
                .remove(&pubkey);
        });
    }

    async fn recycle_connection(
        pubsub_connection: Arc<PubSubConnection>,
        subscriptions: Arc<Mutex<HashMap<Pubkey, AccountSubscription>>>,
        subscription_updates_sender: mpsc::Sender<SubscriptionUpdate>,
        commitment: CommitmentConfig,
        recycle_lock: Arc<AsyncMutex<()>>,
        skip_pubkey: Option<Pubkey>,
    ) -> RemoteAccountProviderResult<()> {
        // Serialize recycle attempts
        let _guard = recycle_lock.lock().await;

        debug!("RecycleConnections: starting recycle process");

        // Recreate the pubsub connection, in case that fails leave it be, as there's not much that can be done about it, next subscription attempt will try to reconnect again
        debug!(
            "RecycleConnections: creating ws connection for {}",
            pubsub_connection.url()
        );

        if let Err(err) = pubsub_connection.reconnect().await {
            error!(
                "RecycleConnections: failed to create ws connection: {err:?}"
            );
            return Err(err.into());
        }

        // Cancel subscriptions except skip_pubkey and collect pubkeys to re-subscribe later
        let mut subs_lock = subscriptions.lock().unwrap();
        let keys_to_recycle: Vec<Pubkey> = subs_lock
            .keys()
            .filter(|pk| skip_pubkey != Some(**pk))
            .cloned()
            .collect();
        debug!(
            "RecycleConnections: cancelling {} subscriptions",
            keys_to_recycle.len(),
        );
        let mut to_resubscribe = HashSet::new();
        for pk in &keys_to_recycle {
            if let Some(AccountSubscription { cancellation_token }) =
                subs_lock.remove(pk)
            {
                to_resubscribe.insert(*pk);
                cancellation_token.cancel();
            }
        }
        debug!(
            "RecycleConnections: cancelled {} subscriptions",
            to_resubscribe.len()
        );

        // Re-subscribe to all accounts
        debug!(
            "RecycleConnections: re-subscribing to {} accounts",
            to_resubscribe.len()
        );
        for pk in to_resubscribe {
            let (tx, _rx) = oneshot::channel();
            Self::add_sub(
                pk,
                tx,
                subscriptions.clone(),
                pubsub_connection.clone(),
                subscription_updates_sender.clone(),
                commitment,
                recycle_lock.clone(),
            );
        }

        debug!("RecycleConnections: completed");

        Ok(())
    }
}
