use std::{
    collections::HashMap,
    fmt,
    sync::{
        atomic::{AtomicBool, AtomicU16, Ordering},
        Arc, Mutex,
    },
};

use log::*;
use solana_account_decoder_client_types::{UiAccount, UiAccountEncoding};
use solana_pubkey::Pubkey;
use solana_rpc_client_api::{
    config::RpcAccountInfoConfig, response::Response as RpcResponse,
};
use solana_sdk::{commitment_config::CommitmentConfig, sysvar::clock};
use tokio::{
    sync::{mpsc, oneshot},
    time::Duration,
};
use tokio_stream::StreamExt;
use tokio_util::sync::CancellationToken;

use super::{
    chain_pubsub_client::PubSubConnection,
    errors::{RemoteAccountProviderError, RemoteAccountProviderResult},
};

// Log every 10 secs (given chain slot time is 400ms)
const CLOCK_LOG_SLOT_FREQ: u64 = 25;

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
    /// The token to use to cancel all subscriptions and shut down the
    /// message listener, essentially shutting down whis actor
    shutdown_token: CancellationToken,
    /// Unique client ID for this actor instance used in logs
    client_id: u16,
    /// Indicates whether the actor is connected or has been disconnected due RPC to connection
    /// issues
    is_connected: Arc<AtomicBool>,
    /// Channel used to signal connection issues to the submux
    abort_sender: mpsc::Sender<()>,
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
    Reconnect {
        response: oneshot::Sender<RemoteAccountProviderResult<()>>,
    },
}

const SUBSCRIPTION_UPDATE_CHANNEL_SIZE: usize = 5_000;
const MESSAGE_CHANNEL_SIZE: usize = 1_000;

impl ChainPubsubActor {
    pub async fn new_from_url(
        pubsub_url: &str,
        abort_sender: mpsc::Sender<()>,
        commitment: CommitmentConfig,
    ) -> RemoteAccountProviderResult<(Self, mpsc::Receiver<SubscriptionUpdate>)>
    {
        let config = PubsubClientConfig::from_url(pubsub_url, commitment);
        Self::new(abort_sender, config).await
    }

    pub async fn new(
        abort_sender: mpsc::Sender<()>,
        pubsub_client_config: PubsubClientConfig,
    ) -> RemoteAccountProviderResult<(Self, mpsc::Receiver<SubscriptionUpdate>)>
    {
        static CLIENT_ID: AtomicU16 = AtomicU16::new(0);

        let url = pubsub_client_config.pubsub_url.clone();
        let pubsub_connection = Arc::new(PubSubConnection::new(url).await?);

        let (subscription_updates_sender, subscription_updates_receiver) =
            mpsc::channel(SUBSCRIPTION_UPDATE_CHANNEL_SIZE);
        let (messages_sender, messages_receiver) =
            mpsc::channel(MESSAGE_CHANNEL_SIZE);

        let shutdown_token = CancellationToken::new();
        let me = Self {
            pubsub_client_config,
            pubsub_connection,
            messages_sender,
            subscriptions: Default::default(),
            subscription_updates_sender,
            shutdown_token,
            client_id: CLIENT_ID.fetch_add(1, Ordering::SeqCst),
            is_connected: Arc::new(AtomicBool::new(true)),
            abort_sender,
        };
        me.start_worker(messages_receiver);

        // Listened on by the client of this actor to receive updates for
        // subscribed accounts
        Ok((me, subscription_updates_receiver))
    }

    pub async fn shutdown(&self) {
        info!(
            "[client_id={}] Shutting down ChainPubsubActor",
            self.client_id
        );
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
        if !self.is_connected.load(Ordering::SeqCst) {
            return 0;
        }
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
        if !self.is_connected.load(Ordering::SeqCst) {
            return vec![];
        }
        let subs = self
            .subscriptions
            .lock()
            .expect("subscriptions lock poisoned");
        subs.keys().copied().collect()
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
        let client_id = self.client_id;
        let is_connected = self.is_connected.clone();
        let abort_sender = self.abort_sender.clone();
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
                                abort_sender.clone(),
                                client_id,
                                is_connected.clone(),
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

    #[allow(clippy::too_many_arguments)]
    async fn handle_msg(
        subscriptions: Arc<Mutex<HashMap<Pubkey, AccountSubscription>>>,
        pubsub_connection: Arc<PubSubConnection>,
        subscription_updates_sender: mpsc::Sender<SubscriptionUpdate>,
        pubsub_client_config: PubsubClientConfig,
        abort_sender: mpsc::Sender<()>,
        client_id: u16,
        is_connected: Arc<AtomicBool>,
        msg: ChainPubsubActorMessage,
    ) {
        fn send_ok(
            response: oneshot::Sender<RemoteAccountProviderResult<()>>,
            client_id: u16,
        ) {
            let _ = response.send(Ok(())).inspect_err(|err| {
                warn!(
                    "[client_id={client_id}]  Failed to send msg ack: {err:?}"
                );
            });
        }

        match msg {
            ChainPubsubActorMessage::AccountSubscribe { pubkey, response } => {
                if !is_connected.load(Ordering::SeqCst) {
                    trace!("[client_id={client_id}] Ignoring subscribe request for {pubkey} because disconnected");
                    send_ok(response, client_id);
                    return;
                }
                let commitment_config = pubsub_client_config.commitment_config;
                Self::add_sub(
                    pubkey,
                    response,
                    subscriptions,
                    pubsub_connection,
                    subscription_updates_sender,
                    abort_sender,
                    is_connected,
                    commitment_config,
                    client_id,
                );
            }
            ChainPubsubActorMessage::AccountUnsubscribe {
                pubkey,
                response,
            } => {
                if !is_connected.load(Ordering::SeqCst) {
                    trace!("[client_id={client_id}] Ignoring unsubscribe request for {pubkey} because disconnected");
                    send_ok(response, client_id);
                    return;
                }
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
            ChainPubsubActorMessage::Reconnect { response } => {
                let result = Self::try_reconnect(
                    pubsub_connection,
                    pubsub_client_config,
                    client_id,
                    is_connected,
                )
                .await;
                let _ = response.send(result);
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
        abort_sender: mpsc::Sender<()>,
        is_connected: Arc<AtomicBool>,
        commitment_config: CommitmentConfig,
        client_id: u16,
    ) {
        if subs
            .lock()
            .expect("subscriptions lock poisoned")
            .contains_key(&pubkey)
        {
            trace!("[client_id={client_id}] Subscription for {pubkey} already exists, ignoring add_sub request");
            let _ = sub_response.send(Ok(()));
            return;
        }

        trace!("[client_id={client_id}] Adding subscription for {pubkey} with commitment {commitment_config:?}");

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
            let (mut update_stream, unsubscribe) = match pubsub_connection
                .account_subscribe(&pubkey, config.clone())
                .await
            {
                Ok(res) => res,
                Err(err) => {
                    error!("[client_id={client_id}] Failed to subscribe to account {pubkey} {err:?}");
                    Self::abort_and_signal_connection_issue(
                        client_id,
                        subs.clone(),
                        abort_sender,
                        is_connected.clone(),
                    );

                    return;
                }
            };

            // RPC succeeded - confirm to the requester that the subscription was made
            let _ = sub_response.send(Ok(()));

            // Now keep listening for updates and relay them to the
            // subscription updates sender until it is cancelled
            loop {
                tokio::select! {
                    _ = cancellation_token.cancelled() => {
                        trace!("[client_id={client_id}] Subscription for {pubkey} was cancelled");
                        break;
                    }
                    update = update_stream.next() => {
                        if let Some(rpc_response) = update {
                            if log_enabled!(log::Level::Trace) && (!pubkey.eq(&clock::ID) ||
                               rpc_response.context.slot % CLOCK_LOG_SLOT_FREQ == 0) {
                                trace!("[client_id={client_id}] Received update for {pubkey}: {rpc_response:?}");
                            }
                            let _ = subscription_updates_sender.send(SubscriptionUpdate {
                                pubkey,
                                rpc_response,
                            }).await.inspect_err(|err| {
                                error!("[client_id={client_id}] Failed to send {pubkey} subscription update: {err:?}");
                            });
                        } else {
                            debug!("[client_id={client_id}] Subscription for {pubkey} ended (EOF); signaling connection issue");
                            Self::abort_and_signal_connection_issue(
                                client_id,
                                subs.clone(),
                                abort_sender.clone(),
                                is_connected.clone(),
                            );
                            // Return early - abort_and_signal_connection_issue cancels all
                            // subscriptions, triggering cleanup via the cancellation path
                            // above. No need to run unsubscribe/cleanup here.
                            return;
                        }
                    }
                }
            }

            // Clean up subscription with timeout to prevent hanging on dead sockets
            if tokio::time::timeout(Duration::from_secs(2), unsubscribe())
                .await
                .is_err()
            {
                warn!(
                    "[client_id={client_id}] unsubscribe timed out for {pubkey}"
                );
            }
            subs.lock()
                .expect("subscriptions lock poisoned")
                .remove(&pubkey);
        });
    }

    async fn try_reconnect(
        pubsub_connection: Arc<PubSubConnection>,
        pubsub_client_config: PubsubClientConfig,
        client_id: u16,
        is_connected: Arc<AtomicBool>,
    ) -> RemoteAccountProviderResult<()> {
        // 1. Try to reconnect the pubsub connection
        if let Err(err) = pubsub_connection.reconnect().await {
            debug!("[client_id={}] failed to reconnect: {err:?}", client_id);
            return Err(err.into());
        }
        // Make a sub to any account and unsub immediately to verify connection
        let pubkey = Pubkey::new_unique();
        let config = RpcAccountInfoConfig {
            commitment: Some(pubsub_client_config.commitment_config),
            encoding: Some(UiAccountEncoding::Base64Zstd),
            ..Default::default()
        };

        // 2. Try to subscribe to an account to verify connection
        let (_, unsubscribe) =
            match pubsub_connection.account_subscribe(&pubkey, config).await {
                Ok(res) => res,
                Err(err) => {
                    error!(
                    "[client_id={}] to verify connection via subscribe {err:?}",
                    client_id
                );
                    return Err(err.into());
                }
            };

        // 3. Unsubscribe immediately
        unsubscribe().await;

        // 4. We are now connected again
        is_connected.store(true, Ordering::SeqCst);
        Ok(())
    }

    fn abort_and_signal_connection_issue(
        client_id: u16,
        subscriptions: Arc<Mutex<HashMap<Pubkey, AccountSubscription>>>,
        abort_sender: mpsc::Sender<()>,
        is_connected: Arc<AtomicBool>,
    ) {
        // Only abort if we were connected; prevents duplicate aborts
        if !is_connected.swap(false, Ordering::SeqCst) {
            trace!(
                "[client_id={client_id}] already disconnected, skipping abort"
            );
            return;
        }

        debug!("[client_id={client_id}] aborting");

        let drained = {
            let mut subs_lock = subscriptions.lock().unwrap();
            std::mem::take(&mut *subs_lock)
        };
        let drained_len = drained.len();
        for (_, AccountSubscription { cancellation_token }) in drained {
            cancellation_token.cancel();
        }
        debug!(
            "[client_id={client_id}] canceled {} subscriptions",
            drained_len
        );
        // Use try_send to avoid blocking and naturally coalesce signals
        let _ = abort_sender.try_send(()).inspect_err(|err| {
            // Channel full is expected when reconnect is already in progress
            if !matches!(err, mpsc::error::TrySendError::Full(_)) {
                error!(
                    "[client_id={client_id}] failed to signal connection issue: {err:?}",
                )
            }
        });
    }
}
