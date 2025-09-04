use log::*;
use solana_rpc_client_api::response::Response as RpcResponse;
use solana_sdk::commitment_config::CommitmentConfig;
use solana_sdk::sysvar::clock;
use std::fmt;
use std::sync::Arc;
use std::{
    collections::{HashMap, HashSet},
    sync::Mutex,
};
use tokio::sync::{mpsc, oneshot};
use tokio_stream::StreamExt;

use solana_account_decoder_client_types::{UiAccount, UiAccountEncoding};
use solana_pubkey::Pubkey;
use solana_pubsub_client::nonblocking::pubsub_client::PubsubClient;
use solana_rpc_client_api::config::RpcAccountInfoConfig;
use tokio_util::sync::CancellationToken;

use super::errors::{RemoteAccountProviderError, RemoteAccountProviderResult};

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
    /// Underlying pubsub client to connect to the chain
    pubsub_client: Arc<PubsubClient>,
    /// Sends subscribe/unsubscribe messages to this actor
    messages_sender: mpsc::Sender<ChainPubsubActorMessage>,
    /// Map of subscriptions we are holding
    subscriptions: Arc<Mutex<HashMap<Pubkey, AccountSubscription>>>,
    /// Sends updates for any account subscription that is received via
    /// the [Self::pubsub_client]
    subscription_updates_sender: mpsc::Sender<SubscriptionUpdate>,
    /// The tasks that watch subscriptions via the [Self::pubsub_client] and
    /// channel them into the [Self::subscription_updates_sender]
    subscription_watchers: Arc<Mutex<tokio::task::JoinSet<()>>>,
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
    RecycleConnections {
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
        let pubsub_client = Arc::new(
            PubsubClient::new(pubsub_client_config.pubsub_url.as_str()).await?,
        );

        let (subscription_updates_sender, subscription_updates_receiver) =
            mpsc::channel(SUBSCRIPTION_UPDATE_CHANNEL_SIZE);
        let (messages_sender, messages_receiver) =
            mpsc::channel(MESSAGE_CHANNEL_SIZE);
        let subscription_watchers =
            Arc::new(Mutex::new(tokio::task::JoinSet::new()));
        let shutdown_token = CancellationToken::new();
        let me = Self {
            pubsub_client_config,
            pubsub_client,
            messages_sender,
            subscriptions: Default::default(),
            subscription_updates_sender,
            subscription_watchers,
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
        // TODO:
        // let mut subs = self.subscription_watchers.lock().unwrap();;
        // subs.join_all().await;
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
        let subscription_watchers = self.subscription_watchers.clone();
        let shutdown_token = self.shutdown_token.clone();
        let pubsub_client_config = self.pubsub_client_config.clone();
        let subscription_updates_sender =
            self.subscription_updates_sender.clone();
        let mut pubsub_client = self.pubsub_client.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    msg = messages_receiver.recv() => {
                        if let Some(msg) = msg {
                            pubsub_client = Self::handle_msg(
                                subs.clone(),
                                pubsub_client.clone(),
                                subscription_watchers.clone(),
                                subscription_updates_sender.clone(),
                                pubsub_client_config.clone(),
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
        pubsub_client: Arc<PubsubClient>,
        subscription_watchers: Arc<Mutex<tokio::task::JoinSet<()>>>,
        subscription_updates_sender: mpsc::Sender<SubscriptionUpdate>,
        pubsub_client_config: PubsubClientConfig,
        msg: ChainPubsubActorMessage,
    ) -> Arc<PubsubClient> {
        match msg {
            ChainPubsubActorMessage::AccountSubscribe { pubkey, response } => {
                let commitment_config = pubsub_client_config.commitment_config;
                Self::add_sub(
                    pubkey,
                    response,
                    subscriptions,
                    pubsub_client.clone(),
                    subscription_watchers,
                    subscription_updates_sender,
                    commitment_config,
                );
                pubsub_client
            }
            ChainPubsubActorMessage::AccountUnsubscribe {
                pubkey,
                response,
            } => {
                if let Some(AccountSubscription { cancellation_token }) =
                    subscriptions.lock().unwrap().remove(&pubkey)
                {
                    cancellation_token.cancel();
                    let _ = response.send(Ok(()));
                } else {
                    let _  = response
                        .send(Err(RemoteAccountProviderError::AccountSubscriptionDoesNotExist(
                            pubkey.to_string(),
                        )));
                }
                pubsub_client
            }
            ChainPubsubActorMessage::RecycleConnections { response } => {
                match Self::recycle_connections(
                    subscriptions,
                    subscription_watchers,
                    subscription_updates_sender,
                    pubsub_client_config,
                )
                .await
                {
                    Ok(new_client) => {
                        let _ = response.send(Ok(()));
                        new_client
                    }
                    Err(err) => {
                        let _ = response.send(Err(err));
                        pubsub_client
                    }
                }
            }
        }
    }

    fn add_sub(
        pubkey: Pubkey,
        sub_response: oneshot::Sender<RemoteAccountProviderResult<()>>,
        subs: Arc<Mutex<HashMap<Pubkey, AccountSubscription>>>,
        pubsub_client: Arc<PubsubClient>,
        subscription_watchers: Arc<Mutex<tokio::task::JoinSet<()>>>,
        subscription_updates_sender: mpsc::Sender<SubscriptionUpdate>,
        commitment_config: CommitmentConfig,
    ) {
        trace!("Adding subscription for {pubkey} with commitment {commitment_config:?}");

        let config = RpcAccountInfoConfig {
            commitment: Some(commitment_config),
            encoding: Some(UiAccountEncoding::Base64Zstd),
            ..Default::default()
        };

        let cancellation_token = CancellationToken::new();

        let mut sub_joinset = subscription_watchers.lock().unwrap();
        sub_joinset.spawn(async move {
            // Attempt to subscribe to the account
            let (mut update_stream, unsubscribe) = match pubsub_client
                .account_subscribe(&pubkey, Some(config))
                .await {
                Ok(res) => res,
                Err(err) => {
                    let _ = sub_response.send(Err(err.into()));
                    return;
                }
            };

            // Then track the subscription and confirm to the requester that the
            // subscription was made
            subs.lock().unwrap().insert(pubkey, AccountSubscription {
                cancellation_token: cancellation_token.clone(),
            });

            let _ = sub_response.send(Ok(()));

            // Now keep listening for updates and relay them to the
            // subscription updates sender until it is cancelled
            loop {
                tokio::select! {
                    _ = cancellation_token.cancelled() => {
                        debug!("Subscription for {pubkey} was cancelled");
                        unsubscribe().await;
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
                            warn!("Subscription for {pubkey} ended by update stream");
                            break;
                        }
                    }
                }
            }
        });
    }

    async fn recycle_connections(
        subscriptions: Arc<Mutex<HashMap<Pubkey, AccountSubscription>>>,
        subscription_watchers: Arc<Mutex<tokio::task::JoinSet<()>>>,
        subscription_updates_sender: mpsc::Sender<SubscriptionUpdate>,
        pubsub_client_config: PubsubClientConfig,
    ) -> RemoteAccountProviderResult<Arc<PubsubClient>> {
        debug!("RecycleConnections: starting recycle process");

        // 1. Recreate the pubsub client, in case that fails leave the old one in place
        //    as this is the best we can do
        debug!(
            "RecycleConnections: creating new PubsubClient for {}",
            pubsub_client_config.pubsub_url
        );
        let new_client = match PubsubClient::new(
            pubsub_client_config.pubsub_url.as_str(),
        )
        .await
        {
            Ok(c) => Arc::new(c),
            Err(err) => {
                error!("RecycleConnections: failed to create new PubsubClient: {err:?}");
                return Err(err.into());
            }
        };

        // Cancel all current subscriptions and collect pubkeys to re-subscribe later
        let drained = {
            let mut subs_lock = subscriptions.lock().unwrap();
            std::mem::take(&mut *subs_lock)
        };
        let mut to_resubscribe = HashSet::new();
        for (pk, AccountSubscription { cancellation_token }) in drained {
            to_resubscribe.insert(pk);
            cancellation_token.cancel();
        }
        debug!(
            "RecycleConnections: cancelled {} subscriptions",
            to_resubscribe.len()
        );

        // Abort and await all watcher tasks and add fresh joinset
        debug!("RecycleConnections: aborting watcher tasks");
        let mut old_joinset = {
            let mut watchers = subscription_watchers
                .lock()
                .expect("subscription_watchers lock poisonde");
            std::mem::replace(&mut *watchers, tokio::task::JoinSet::new())
        };
        old_joinset.abort_all();
        while let Some(_res) = old_joinset.join_next().await {}
        debug!("RecycleConnections: watcher tasks terminated");

        // Re-subscribe to all accounts
        debug!(
            "RecycleConnections: re-subscribing to {} accounts",
            to_resubscribe.len()
        );
        let commitment_config = pubsub_client_config.commitment_config;
        for pk in to_resubscribe {
            let (tx, _rx) = oneshot::channel();
            Self::add_sub(
                pk,
                tx,
                subscriptions.clone(),
                new_client.clone(),
                subscription_watchers.clone(),
                subscription_updates_sender.clone(),
                commitment_config,
            );
        }

        debug!("RecycleConnections: completed");

        Ok(new_client)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        skip_if_no_test_validator,
        testing::utils::{
            airdrop, init_logger, random_pubkey, PUBSUB_URL, RPC_URL,
        },
    };
    use solana_pubkey::Pubkey;
    use solana_rpc_client::nonblocking::rpc_client::RpcClient;
    use solana_sdk::commitment_config::CommitmentConfig;
    use tokio::sync::{mpsc, oneshot};
    use tokio::time::{timeout, Duration, Instant};

    async fn expect_update_for(
        updates: &mut mpsc::Receiver<SubscriptionUpdate>,
        target: Pubkey,
    ) -> SubscriptionUpdate {
        loop {
            let maybe = timeout(Duration::from_millis(1500), updates.recv())
                .await
                .expect("timed out waiting for subscription update");
            let update = maybe.expect("subscription updates channel closed");
            if update.pubkey == target {
                return update;
            }
        }
    }

    async fn setup_actor_and_client() -> (
        ChainPubsubActor,
        mpsc::Receiver<SubscriptionUpdate>,
        RpcClient,
    ) {
        let (actor, updates_rx) = ChainPubsubActor::new_from_url(
            PUBSUB_URL,
            CommitmentConfig::confirmed(),
        )
        .await
        .expect("failed to create ChainPubsubActor");
        let rpc_client = RpcClient::new(RPC_URL.to_string());
        (actor, updates_rx, rpc_client)
    }

    async fn subscribe(actor: &ChainPubsubActor, pubkey: Pubkey) {
        let (tx, rx) = oneshot::channel();
        actor
            .send_msg(ChainPubsubActorMessage::AccountSubscribe {
                pubkey,
                response: tx,
            })
            .await
            .expect("failed to send AccountSubscribe message");
        rx.await
            .expect("subscribe ack channel dropped")
            .expect("subscribe failed");
    }

    async fn unsubscribe(actor: &ChainPubsubActor, pubkey: Pubkey) {
        let (tx, rx) = oneshot::channel();
        actor
            .send_msg(ChainPubsubActorMessage::AccountUnsubscribe {
                pubkey,
                response: tx,
            })
            .await
            .expect("failed to send AccountUnsubscribe message");
        rx.await
            .expect("unsubscribe ack channel dropped")
            .expect("unsubscribe failed");
    }

    async fn recycle(actor: &ChainPubsubActor) {
        let (tx, rx) = oneshot::channel();
        actor
            .send_msg(ChainPubsubActorMessage::RecycleConnections {
                response: tx,
            })
            .await
            .expect("failed to send RecycleConnections message");
        rx.await
            .expect("recycle ack channel dropped")
            .expect("recycle failed");
    }

    async fn airdrop_and_expect_update(
        rpc_client: &RpcClient,
        updates: &mut mpsc::Receiver<SubscriptionUpdate>,
        pubkey: Pubkey,
        lamports: u64,
    ) -> SubscriptionUpdate {
        airdrop(rpc_client, &pubkey, lamports).await;
        expect_update_for(updates, pubkey).await
    }

    async fn expect_no_update_for(
        updates: &mut mpsc::Receiver<SubscriptionUpdate>,
        target: Pubkey,
        timeout_ms: u64,
    ) {
        let deadline = Instant::now() + Duration::from_millis(timeout_ms);
        loop {
            let now = Instant::now();
            if now >= deadline {
                break;
            }
            let remaining = deadline.saturating_duration_since(now);
            match timeout(remaining, updates.recv()).await {
                Ok(Some(update)) => {
                    if update.pubkey == target {
                        panic!(
                            "unexpected update for unsubscribed account {target}"
                        );
                    }
                    // ignore other updates and keep waiting
                }
                Ok(None) => panic!("subscription updates channel closed"),
                Err(_) => break, // timed out => success
            }
        }
    }

    #[tokio::test]
    async fn ixtest_recycle_connections() {
        init_logger();
        skip_if_no_test_validator!();

        // 1. Create actor and RPC client with confirmed commitment
        let (actor, mut updates_rx, rpc_client) =
            setup_actor_and_client().await;

        // 2. Create account via airdrop
        let pubkey = random_pubkey();
        airdrop(&rpc_client, &pubkey, 1_000_000).await;

        // 3. Subscribe to that account
        subscribe(&actor, pubkey).await;

        // 4. Airdrop again and ensure we receive the update
        let _first_update = airdrop_and_expect_update(
            &rpc_client,
            &mut updates_rx,
            pubkey,
            2_000_000,
        )
        .await;

        // 5. Recycle connections
        recycle(&actor).await;

        // 6. Airdrop again and ensure we receive the update again
        let _second_update = airdrop_and_expect_update(
            &rpc_client,
            &mut updates_rx,
            pubkey,
            3_000_000,
        )
        .await;

        // Cleanup
        actor.shutdown().await;
    }

    #[tokio::test]
    async fn ixtest_recycle_connections_multiple_accounts() {
        init_logger();
        skip_if_no_test_validator!();

        // Setup
        let (actor, mut updates_rx, rpc_client) =
            setup_actor_and_client().await;

        // Create 4 accounts and fund them once to ensure existence
        let pks = [
            random_pubkey(),
            random_pubkey(),
            random_pubkey(),
            random_pubkey(),
        ];
        for pk in &pks {
            airdrop(&rpc_client, pk, 1_000_000).await;
        }

        // Subscribe to all 4
        for &pk in &pks {
            subscribe(&actor, pk).await;
        }

        // Airdrop to each and ensure we receive updates for all
        for &pk in &pks {
            let _ = airdrop_and_expect_update(
                &rpc_client,
                &mut updates_rx,
                pk,
                2_000_000,
            )
            .await;
        }

        // Unsubscribe from the 4th
        let unsub_pk = pks[3];
        unsubscribe(&actor, unsub_pk).await;

        // Recycle connections
        recycle(&actor).await;

        // Airdrop to first three and expect updates
        for &pk in &pks[0..3] {
            let _ = airdrop_and_expect_update(
                &rpc_client,
                &mut updates_rx,
                pk,
                3_000_000,
            )
            .await;
        }

        // Airdrop to the 4th and ensure we do NOT receive an update for it
        airdrop(&rpc_client, &unsub_pk, 3_000_000).await;
        expect_no_update_for(&mut updates_rx, unsub_pk, 1500).await;

        // Cleanup
        actor.shutdown().await;
    }
}
