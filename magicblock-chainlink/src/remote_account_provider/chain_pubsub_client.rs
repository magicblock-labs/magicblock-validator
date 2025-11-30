use std::{
    collections::HashSet,
    mem,
    sync::{Arc, Mutex},
    time::Duration,
};

use arc_swap::ArcSwap;
use async_trait::async_trait;
use futures_util::{future::BoxFuture, stream::BoxStream};
use log::*;
use solana_account_decoder::UiAccount;
use solana_pubkey::Pubkey;
use solana_pubsub_client::nonblocking::pubsub_client::{
    PubsubClient, PubsubClientResult,
};
use solana_rpc_client_api::{config::RpcAccountInfoConfig, response::Response};
use solana_sdk::commitment_config::CommitmentConfig;
use tokio::{
    sync::{mpsc, oneshot, Mutex as AsyncMutex},
    time,
};

use super::{
    chain_pubsub_actor::{
        ChainPubsubActor, ChainPubsubActorMessage, SubscriptionUpdate,
    },
    errors::RemoteAccountProviderResult,
};

type UnsubscribeFn = Box<dyn FnOnce() -> BoxFuture<'static, ()> + Send>;
type SubscribeResult = PubsubClientResult<(
    BoxStream<'static, Response<UiAccount>>,
    UnsubscribeFn,
)>;

const MAX_RECONNECT_ATTEMPTS: usize = 5;
const RECONNECT_ATTEMPT_DELAY: Duration = Duration::from_millis(500);

pub struct PubSubConnection {
    client: ArcSwap<PubsubClient>,
    url: String,
    reconnect_guard: AsyncMutex<()>,
}

impl PubSubConnection {
    pub async fn new(url: String) -> RemoteAccountProviderResult<Self> {
        let client = Arc::new(PubsubClient::new(&url).await?).into();
        let reconnect_guard = AsyncMutex::new(());
        Ok(Self {
            client,
            url,
            reconnect_guard,
        })
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    pub async fn account_subscribe(
        &self,
        pubkey: &Pubkey,
        config: RpcAccountInfoConfig,
    ) -> SubscribeResult {
        let client = self.client.load();
        let config = Some(config.clone());
        let (stream, unsub) = client.account_subscribe(pubkey, config).await?;
        // SAFETY:
        // the returned stream depends on the used client, which is only ever dropped
        // if the connection has been terminated, at which point the stream is useless
        // and will be discarded as well, thus it's safe lifetime extension to 'static
        let stream = unsafe {
            mem::transmute::<
                BoxStream<'_, Response<UiAccount>>,
                BoxStream<'static, Response<UiAccount>>,
            >(stream)
        };
        Ok((stream, unsub))
    }

    pub async fn reconnect(&self) -> PubsubClientResult<()> {
        // Prevents multiple reconnect attempts running concurrently
        let _guard = match self.reconnect_guard.try_lock() {
            Ok(g) => g,
            // Reconnect is already in progress
            Err(_) => {
                // Wait a bit and return to retry subscription
                time::sleep(RECONNECT_ATTEMPT_DELAY).await;
                return Ok(());
            }
        };
        let mut attempt = 1;
        let client = loop {
            match PubsubClient::new(&self.url).await {
                Ok(c) => break Arc::new(c),
                Err(error) => {
                    warn!(
                        "failed to reconnect to ws endpoint at {} {error}",
                        self.url
                    );
                    if attempt == MAX_RECONNECT_ATTEMPTS {
                        return Err(error);
                    }
                    attempt += 1;
                    time::sleep(RECONNECT_ATTEMPT_DELAY).await;
                }
            }
        };
        self.client.store(client);
        Ok(())
    }
}

// -----------------
// Trait
// -----------------
#[async_trait]
pub trait ChainPubsubClient: Send + Sync + Clone + 'static {
    async fn subscribe(
        &self,
        pubkey: Pubkey,
    ) -> RemoteAccountProviderResult<()>;
    async fn unsubscribe(
        &self,
        pubkey: Pubkey,
    ) -> RemoteAccountProviderResult<()>;
    async fn shutdown(&self);

    fn take_updates(&self) -> mpsc::Receiver<SubscriptionUpdate>;

    /// Provides the total number of subscriptions and the number of
    /// subscriptions when excludig pubkeys in `exclude`.
    /// - `exclude`: Optional slice of pubkeys to exclude from the count.
    /// Returns a tuple of (total subscriptions, filtered subscriptions).
    async fn subscription_count(
        &self,
        exclude: Option<&[Pubkey]>,
    ) -> (usize, usize);

    fn subscriptions(&self) -> Vec<Pubkey>;
}

#[async_trait]
pub trait ReconnectableClient {
    /// Attempts to reconnect to the pubsub server and should be invoked when the client sent the
    /// abort signal.
    async fn try_reconnect(&self) -> RemoteAccountProviderResult<()>;
    /// Re-subscribes to multiple accounts after a reconnection.
    async fn resub_multiple(
        &self,
        pubkeys: HashSet<Pubkey>,
    ) -> RemoteAccountProviderResult<()>;
}

// -----------------
// Implementation
// -----------------
#[derive(Clone)]
pub struct ChainPubsubClientImpl {
    actor: Arc<ChainPubsubActor>,
    updates_rcvr: Arc<Mutex<Option<mpsc::Receiver<SubscriptionUpdate>>>>,
}

impl ChainPubsubClientImpl {
    pub async fn try_new_from_url(
        pubsub_url: &str,
        abort_sender: mpsc::Sender<()>,
        commitment: CommitmentConfig,
    ) -> RemoteAccountProviderResult<Self> {
        let (actor, updates) = ChainPubsubActor::new_from_url(
            pubsub_url,
            abort_sender,
            commitment,
        )
        .await?;
        Ok(Self {
            actor: Arc::new(actor),
            updates_rcvr: Arc::new(Mutex::new(Some(updates))),
        })
    }
}

#[async_trait]
impl ChainPubsubClient for ChainPubsubClientImpl {
    async fn shutdown(&self) {
        self.actor.shutdown().await;
    }

    fn take_updates(&self) -> mpsc::Receiver<SubscriptionUpdate> {
        // SAFETY: This can only be None if `take_updates` is called more than
        // once (double-take). That indicates a logic bug in the calling code.
        // Panicking here surfaces the bug early and prevents silently losing
        // the updates stream.
        self.updates_rcvr
            .lock()
            .unwrap()
            .take()
            .expect("ChainPubsubClientImpl::take_updates called more than once")
    }

    async fn subscribe(
        &self,
        pubkey: Pubkey,
    ) -> RemoteAccountProviderResult<()> {
        let (tx, rx) = oneshot::channel();
        self.actor
            .send_msg(ChainPubsubActorMessage::AccountSubscribe {
                pubkey,
                response: tx,
            })
            .await?;

        rx.await
            .inspect_err(|err| {
                warn!("ChainPubsubClientImpl::subscribe - RecvError occurred while awaiting subscription response for {}: {err:?}. This indicates the actor sender was dropped without responding.", pubkey);
            })?
    }

    async fn unsubscribe(
        &self,
        pubkey: Pubkey,
    ) -> RemoteAccountProviderResult<()> {
        let (tx, rx) = oneshot::channel();
        self.actor
            .send_msg(ChainPubsubActorMessage::AccountUnsubscribe {
                pubkey,
                response: tx,
            })
            .await?;

        rx.await
            .inspect_err(|err| {
                warn!("ChainPubsubClientImpl::unsubscribe - RecvError occurred while awaiting unsubscription response for {}: {err:?}. This indicates the actor sender was dropped without responding.", pubkey);
            })?
    }

    async fn subscription_count(
        &self,
        exclude: Option<&[Pubkey]>,
    ) -> (usize, usize) {
        let total = self.actor.subscription_count(&[]);
        let filtered = if let Some(exclude) = exclude {
            self.actor.subscription_count(exclude)
        } else {
            total
        };
        (total, filtered)
    }

    fn subscriptions(&self) -> Vec<Pubkey> {
        self.actor.subscriptions()
    }
}

#[async_trait]
impl ReconnectableClient for ChainPubsubClientImpl {
    async fn try_reconnect(&self) -> RemoteAccountProviderResult<()> {
        let (tx, rx) = oneshot::channel();
        self.actor
            .send_msg(ChainPubsubActorMessage::Reconnect { response: tx })
            .await?;

        rx.await.inspect_err(|err| {
            warn!("RecvError occurred while awaiting reconnect response: {err:?}.");
        })?
    }

    async fn resub_multiple(
        &self,
        pubkeys: HashSet<Pubkey>,
    ) -> RemoteAccountProviderResult<()> {
        for pubkey in pubkeys {
            self.subscribe(pubkey).await?;
            // Don't spam the RPC provider - for 5,000 accounts we would take 250 secs = ~4 minutes
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        Ok(())
    }
}

// -----------------
// Mock
// -----------------
#[cfg(any(test, feature = "dev-context"))]
pub mod mock {
    use std::{collections::HashSet, sync::Mutex, time::Duration};

    use log::*;
    use solana_account::Account;
    use solana_account_decoder::{encode_ui_account, UiAccountEncoding};
    use solana_rpc_client_api::response::{
        Response as RpcResponse, RpcResponseContext,
    };
    use solana_sdk::clock::Slot;

    use super::*;
    use crate::remote_account_provider::{
        RemoteAccountProviderError, RemoteAccountProviderResult,
    };

    #[derive(Clone)]
    pub struct ChainPubsubClientMock {
        updates_sndr: mpsc::Sender<SubscriptionUpdate>,
        updates_rcvr: Arc<Mutex<Option<mpsc::Receiver<SubscriptionUpdate>>>>,
        subscribed_pubkeys: Arc<Mutex<HashSet<Pubkey>>>,
        connected: Arc<Mutex<bool>>,
        pending_resubscribe_failures: Arc<Mutex<usize>>,
    }

    impl ChainPubsubClientMock {
        pub fn new(
            updates_sndr: mpsc::Sender<SubscriptionUpdate>,
            updates_rcvr: mpsc::Receiver<SubscriptionUpdate>,
        ) -> Self {
            Self {
                updates_sndr,
                updates_rcvr: Arc::new(Mutex::new(Some(updates_rcvr))),
                subscribed_pubkeys: Arc::new(Mutex::new(HashSet::new())),
                connected: Arc::new(Mutex::new(true)),
                pending_resubscribe_failures: Arc::new(Mutex::new(0)),
            }
        }

        /// Simulate a disconnect: clear all subscriptions and mark client as disconnected.
        pub fn simulate_disconnect(&self) {
            *self.connected.lock().unwrap() = false;
            self.subscribed_pubkeys.lock().unwrap().clear();
        }

        /// Fail the next N resubscription attempts in resub_multiple().
        pub fn fail_next_resubscriptions(&self, n: usize) {
            *self.pending_resubscribe_failures.lock().unwrap() = n;
        }

        async fn send(&self, update: SubscriptionUpdate) {
            let subscribed_pubkeys =
                self.subscribed_pubkeys.lock().unwrap().clone();
            if subscribed_pubkeys.contains(&update.pubkey) {
                let _ =
                    self.updates_sndr.send(update).await.inspect_err(|err| {
                        error!("Failed to send subscription update: {err:?}")
                    });
            }
        }

        pub async fn send_account_update(
            &self,
            pubkey: Pubkey,
            slot: Slot,
            account: &Account,
        ) {
            let ui_acc = encode_ui_account(
                &pubkey,
                account,
                UiAccountEncoding::Base58,
                None,
                None,
            );
            let rpc_response = RpcResponse {
                context: RpcResponseContext {
                    slot,
                    api_version: None,
                },
                value: ui_acc,
            };
            self.send(SubscriptionUpdate {
                pubkey,
                rpc_response,
            })
            .await;
        }
    }

    #[async_trait]
    impl ChainPubsubClient for ChainPubsubClientMock {
        fn take_updates(&self) -> mpsc::Receiver<SubscriptionUpdate> {
            // SAFETY: This can only be None if `take_updates` is called more
            // than once (double take). That would indicate a logic bug in the
            // calling code. Panicking here surfaces such a bug early and avoids
            // silently losing the updates stream.
            self.updates_rcvr.lock().unwrap().take().expect(
                "ChainPubsubClientMock::take_updates called more than once",
            )
        }
        async fn subscribe(
            &self,
            pubkey: Pubkey,
        ) -> RemoteAccountProviderResult<()> {
            if !*self.connected.lock().unwrap() {
                return Err(
                    RemoteAccountProviderError::AccountSubscriptionsTaskFailed(
                        "mock: subscribe while disconnected".to_string(),
                    ),
                );
            }
            let mut subscribed_pubkeys =
                self.subscribed_pubkeys.lock().unwrap();
            subscribed_pubkeys.insert(pubkey);
            Ok(())
        }

        async fn unsubscribe(
            &self,
            pubkey: Pubkey,
        ) -> RemoteAccountProviderResult<()> {
            let mut subscribed_pubkeys =
                self.subscribed_pubkeys.lock().unwrap();
            subscribed_pubkeys.remove(&pubkey);
            Ok(())
        }

        async fn shutdown(&self) {}

        async fn subscription_count(
            &self,
            exclude: Option<&[Pubkey]>,
        ) -> (usize, usize) {
            let pubkeys: Vec<Pubkey> = {
                let subs = self.subscribed_pubkeys.lock().unwrap();
                subs.iter().cloned().collect()
            };
            let total = pubkeys.len();
            let exclude = exclude.unwrap_or_default();
            let filtered = pubkeys
                .iter()
                .filter(|pubkey| !exclude.contains(pubkey))
                .count();
            (total, filtered)
        }

        fn subscriptions(&self) -> Vec<Pubkey> {
            let subs = self.subscribed_pubkeys.lock().unwrap();
            subs.iter().copied().collect()
        }
    }

    #[async_trait]
    impl ReconnectableClient for ChainPubsubClientMock {
        async fn try_reconnect(&self) -> RemoteAccountProviderResult<()> {
            *self.connected.lock().unwrap() = true;
            Ok(())
        }

        async fn resub_multiple(
            &self,
            pubkeys: HashSet<Pubkey>,
        ) -> RemoteAccountProviderResult<()> {
            // Simulate transient resubscription failures
            {
                let mut to_fail =
                    self.pending_resubscribe_failures.lock().unwrap();
                if *to_fail > 0 {
                    *to_fail -= 1;
                    return Err(
                        RemoteAccountProviderError::AccountSubscriptionsTaskFailed(
                            "mock: forced resubscribe failure".to_string(),
                        ),
                    );
                }
            }
            for pubkey in pubkeys {
                self.subscribe(pubkey).await?;
                // keep it small; tests shouldn't take long
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            Ok(())
        }
    }
}
