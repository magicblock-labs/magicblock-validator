use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use log::*;
use solana_pubkey::Pubkey;
use solana_sdk::commitment_config::CommitmentConfig;
use tokio::sync::{mpsc, oneshot};

use super::{
    chain_pubsub_actor::{
        ChainPubsubActor, ChainPubsubActorMessage, SubscriptionUpdate,
    },
    errors::RemoteAccountProviderResult,
};

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
    async fn recycle_connections(&self);

    fn take_updates(&self) -> mpsc::Receiver<SubscriptionUpdate>;

    /// Provides the total number of subscriptions and the number of
    /// subscriptions when excludig pubkeys in `exclude`.
    /// - `exclude`: Optional slice of pubkeys to exclude from the count.
    /// Returns a tuple of (total subscriptions, filtered subscriptions).
    async fn subscription_count(
        &self,
        exclude: Option<&[Pubkey]>,
    ) -> (usize, usize);
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

    async fn recycle_connections(&self) {
        // Fire a recycle request to the actor and await the acknowledgement.
        // If recycle fails there is nothing the caller could do, so we log an error instead
        let (tx, rx) = oneshot::channel();
        if let Err(err) = self
            .actor
            .send_msg(ChainPubsubActorMessage::RecycleConnections {
                response: tx,
            })
            .await
        {
            error!(
                "ChainPubsubClientImpl::recycle_connections: failed to send RecycleConnections: {err:?}"
            );
            return;
        }
        let res = match rx.await {
            Ok(r) => r,
            Err(err) => {
                error!(
                    "ChainPubsubClientImpl::recycle_connections: actor dropped recycle ack: {err:?}"
                );
                return;
            }
        };
        if let Err(err) = res {
            error!(
                "ChainPubsubClientImpl::recycle_connections: recycle failed: {err:?}"
            );
        }
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
        async fn recycle_connections(&self) {
            self.recycle_calls.fetch_add(1, Ordering::SeqCst);
        }

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
    }
}
