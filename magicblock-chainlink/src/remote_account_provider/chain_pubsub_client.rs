use std::{
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

    async fn subscription_count(&self) -> usize;
}

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
        let stream = unsafe { mem::transmute(stream) };
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
        commitment: CommitmentConfig,
    ) -> RemoteAccountProviderResult<Self> {
        let (actor, updates) =
            ChainPubsubActor::new_from_url(pubsub_url, commitment).await?;
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

        rx.await?
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

        rx.await?
    }

    async fn subscription_count(&self) -> usize {
        self.actor.subscription_count()
    }
}

// -----------------
// Mock
// -----------------
#[cfg(any(test, feature = "dev-context"))]
pub mod mock {
    use std::{
        collections::HashSet,
        sync::{
            atomic::{AtomicU64, Ordering},
            Mutex,
        },
    };

    use log::*;
    use solana_account::Account;
    use solana_account_decoder::{encode_ui_account, UiAccountEncoding};
    use solana_rpc_client_api::response::{
        Response as RpcResponse, RpcResponseContext,
    };
    use solana_sdk::clock::Slot;

    use super::*;

    #[derive(Clone)]
    pub struct ChainPubsubClientMock {
        updates_sndr: mpsc::Sender<SubscriptionUpdate>,
        updates_rcvr: Arc<Mutex<Option<mpsc::Receiver<SubscriptionUpdate>>>>,
        subscribed_pubkeys: Arc<Mutex<HashSet<Pubkey>>>,
        recycle_calls: Arc<AtomicU64>,
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
                recycle_calls: Arc::new(AtomicU64::new(0)),
            }
        }

        pub fn recycle_calls(&self) -> u64 {
            self.recycle_calls.load(Ordering::SeqCst)
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

        async fn subscription_count(&self) -> usize {
            self.subscribed_pubkeys.lock().unwrap().len()
        }
    }
}
