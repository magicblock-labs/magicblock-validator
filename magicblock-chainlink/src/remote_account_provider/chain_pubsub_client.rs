use std::{
    collections::HashSet,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

use async_trait::async_trait;
use magicblock_metrics::metrics;
use solana_commitment_config::CommitmentConfig;
use solana_pubkey::Pubkey;
use tokio::sync::{mpsc, oneshot};
use tracing::*;

use super::{
    chain_pubsub_actor::ChainPubsubActor,
    errors::RemoteAccountProviderResult,
    pubsub_common::{ChainPubsubActorMessage, SubscriptionUpdate},
};

const MAX_RESUB_DELAY_MS: u64 = 800;

// -----------------
// Trait
// -----------------
#[async_trait]
pub trait ChainPubsubClient: Send + Sync + Clone + 'static {
    async fn subscribe(
        &self,
        pubkey: Pubkey,
        retries: Option<usize>,
    ) -> RemoteAccountProviderResult<()>;
    async fn subscribe_program(
        &self,
        program_id: Pubkey,
    ) -> RemoteAccountProviderResult<()>;
    async fn unsubscribe(
        &self,
        pubkey: Pubkey,
    ) -> RemoteAccountProviderResult<()>;
    async fn shutdown(&self) -> RemoteAccountProviderResult<()>;

    fn take_updates(&self) -> mpsc::Receiver<SubscriptionUpdate>;

    /// Provides the total number of subscriptions and the number of
    /// subscriptions when excludig pubkeys in `exclude`.
    /// - `exclude`: Optional slice of pubkeys to exclude from the count.
    /// Returns a tuple of (total subscriptions, filtered subscriptions).
    async fn subscription_count(
        &self,
        exclude: Option<&[Pubkey]>,
    ) -> Option<(usize, usize)>;

    fn subscriptions_union(&self) -> Option<HashSet<Pubkey>>;

    fn subs_immediately(&self) -> bool;

    fn id(&self) -> &str;
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
    /// Returns the current resubscription delay in milliseconds.
    /// Returns None if this client doesn't track resubscription delay.
    fn current_resub_delay_ms(&self) -> Option<u64> {
        None
    }
}

// -----------------
// Implementation
// -----------------
#[derive(Clone)]
pub struct ChainPubsubClientImpl {
    actor: Arc<ChainPubsubActor>,
    updates_rcvr: Arc<Mutex<Option<mpsc::Receiver<SubscriptionUpdate>>>>,
    client_id: String,
    current_resub_delay_ms: Arc<AtomicU64>,
}

impl ChainPubsubClientImpl {
    pub async fn try_new_from_url(
        pubsub_url: &str,
        client_id: String,
        abort_sender: mpsc::Sender<()>,
        commitment: CommitmentConfig,
        resubscription_delay: Duration,
    ) -> RemoteAccountProviderResult<Self> {
        let (actor, updates) = ChainPubsubActor::new_from_url(
            pubsub_url,
            &client_id,
            abort_sender,
            commitment,
        )
        .await?;
        let current_resub_delay_ms =
            Arc::new(AtomicU64::new(resubscription_delay.as_millis() as u64));
        Ok(Self {
            actor: Arc::new(actor),
            updates_rcvr: Arc::new(Mutex::new(Some(updates))),
            client_id,
            current_resub_delay_ms,
        })
    }
}

#[async_trait]
impl ChainPubsubClient for ChainPubsubClientImpl {
    async fn shutdown(&self) -> RemoteAccountProviderResult<()> {
        let (tx, rx) = oneshot::channel();
        self.actor
            .send_msg(ChainPubsubActorMessage::Shutdown { response: tx })
            .await?;

        rx.await.inspect_err(|err| {
            warn!(
                "ChainPubsubClientImpl::shutdown - RecvError \
                     occurred while awaiting shutdown response: {err:?}"
            );
        })?
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
        retries: Option<usize>,
    ) -> RemoteAccountProviderResult<()> {
        let (tx, rx) = oneshot::channel();
        self.actor
            .send_msg(ChainPubsubActorMessage::AccountSubscribe {
                pubkey,
                retries,
                response: tx,
            })
            .await?;

        rx.await
            .inspect_err(|err| {
                warn!(pubkey = %pubkey, error = ?err, "ChainPubsubClientImpl::subscribe - RecvError awaiting subscription response, actor sender dropped");
            })?
    }

    async fn subscribe_program(
        &self,
        program_id: Pubkey,
    ) -> RemoteAccountProviderResult<()> {
        let (tx, rx) = oneshot::channel();
        self.actor
            .send_msg(ChainPubsubActorMessage::ProgramSubscribe {
                pubkey: program_id,
                response: tx,
            })
            .await?;

        rx.await
            .inspect_err(|err| {
                warn!(program_id = %program_id, error = ?err, "ChainPubsubClientImpl::subscribe_program - RecvError awaiting subscription response, actor sender dropped");
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
                warn!(pubkey = %pubkey, error = ?err, "ChainPubsubClientImpl::unsubscribe - RecvError awaiting unsubscription response, actor sender dropped");
            })?
    }

    async fn subscription_count(
        &self,
        exclude: Option<&[Pubkey]>,
    ) -> Option<(usize, usize)> {
        let total = self.actor.subscription_count(&[]);
        let filtered = if let Some(exclude) = exclude {
            self.actor.subscription_count(exclude)
        } else {
            total
        };
        Some((total, filtered))
    }

    fn subscriptions_union(&self) -> Option<HashSet<Pubkey>> {
        Some(self.actor.subscriptions())
    }

    fn subs_immediately(&self) -> bool {
        true
    }

    fn id(&self) -> &str {
        &self.client_id
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
            warn!(error = ?err, "RecvError awaiting reconnect response");
        })?
    }

    async fn resub_multiple(
        &self,
        pubkeys: HashSet<Pubkey>,
    ) -> RemoteAccountProviderResult<()> {
        const RESUB_MULTIPLE_RETRY_PER_PUBKEY: usize = 5;
        let delay_ms = self.current_resub_delay_ms.load(Ordering::SeqCst);
        let delay = Duration::from_millis(delay_ms);
        let pubkeys_vec: Vec<Pubkey> = pubkeys.into_iter().collect();
        for (idx, pubkey) in pubkeys_vec.iter().enumerate() {
            if let Err(err) = self
                .subscribe(*pubkey, Some(RESUB_MULTIPLE_RETRY_PER_PUBKEY))
                .await
            {
                // Report the number of subscriptions we managed before failing
                metrics::set_pubsub_client_resubscribed_count(
                    &self.client_id,
                    idx + 1,
                );
                // Exponentially back off on resubscription attempts, so the next time we
                // reconnect and try to resubscribe, we wait longer in between each subscription
                // in order to avoid overwhelming the RPC with requests
                let new_delay =
                    delay_ms.saturating_mul(2).min(MAX_RESUB_DELAY_MS);
                self.current_resub_delay_ms
                    .store(new_delay, Ordering::SeqCst);
                debug!(
                    error = ?err,
                    total_subs = pubkeys_vec.len(),
                    processed_subs = idx + 1,
                    pubkey = %pubkey,
                    "Re-subscription for multiple pubkeys failed to complete",
                );
                return Err(err);
            }
            // Only sleep between subscriptions, not after the final one
            if idx < pubkeys_vec.len() - 1 {
                tokio::time::sleep(delay).await;
            }
        }
        // Report successful resubscription of all pubkeys
        metrics::set_pubsub_client_resubscribed_count(
            &self.client_id,
            pubkeys_vec.len(),
        );
        Ok(())
    }

    fn current_resub_delay_ms(&self) -> Option<u64> {
        Some(self.current_resub_delay_ms.load(Ordering::SeqCst))
    }
}

// -----------------
// Mock
// -----------------
#[cfg(any(test, feature = "dev-context"))]
pub mod mock {
    use std::{collections::HashSet, time::Duration};

    use parking_lot::Mutex;
    use solana_account::Account;
    use solana_account_decoder::{encode_ui_account, UiAccountEncoding};
    use solana_program::clock::Slot;
    use solana_rpc_client_api::response::{
        Response as RpcResponse, RpcResponseContext,
    };
    use tracing::*;

    use super::*;
    use crate::remote_account_provider::{
        RemoteAccountProviderError, RemoteAccountProviderResult,
    };

    #[derive(Clone)]
    pub struct ChainPubsubClientMock {
        updates_sndr: mpsc::Sender<SubscriptionUpdate>,
        updates_rcvr: Arc<Mutex<Option<mpsc::Receiver<SubscriptionUpdate>>>>,
        subscribed_pubkeys: Arc<Mutex<HashSet<Pubkey>>>,
        subscribed_programs: Arc<Mutex<HashSet<Pubkey>>>,
        subscription_count_at_disconnect: Arc<Mutex<usize>>,
        connected: Arc<Mutex<bool>>,
        pending_resubscribe_failures: Arc<Mutex<usize>>,
        reconnectable: Arc<Mutex<bool>>,
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
                subscribed_programs: Arc::new(Mutex::new(HashSet::new())),
                subscription_count_at_disconnect: Arc::new(Mutex::new(0)),
                connected: Arc::new(Mutex::new(true)),
                pending_resubscribe_failures: Arc::new(Mutex::new(0)),
                reconnectable: Arc::new(Mutex::new(true)),
            }
        }

        /// Simulate a disconnect: clear all subscriptions and mark client as disconnected.
        pub fn simulate_disconnect(&self) {
            *self.connected.lock() = false;
            *self.subscription_count_at_disconnect.lock() =
                self.subscribed_pubkeys.lock().len();
            self.subscribed_pubkeys.lock().clear();
        }

        /// Fail the next N resubscription attempts in resub_multiple().
        pub fn fail_next_resubscriptions(&self, n: usize) {
            *self.pending_resubscribe_failures.lock() = n;
        }

        async fn send(&self, update: SubscriptionUpdate) {
            let subscribed_pubkeys = self.subscribed_pubkeys.lock().clone();
            if subscribed_pubkeys.contains(&update.pubkey) {
                let _ =
                    self.updates_sndr.send(update).await.inspect_err(|err| {
                        error!(error = ?err, "Failed to send subscription update")
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
            let update = SubscriptionUpdate::from((pubkey, rpc_response));
            self.send(update).await;
        }

        pub fn disable_reconnect(&self) {
            *self.reconnectable.lock() = false;
        }

        pub fn enable_reconnect(&self) {
            *self.reconnectable.lock() = true;
        }

        pub fn is_connected_and_resubscribed(&self) -> bool {
            *self.connected.lock()
                && self.subscribed_pubkeys.lock().len()
                    == *self.subscription_count_at_disconnect.lock()
        }

        pub fn subscribed_program_ids(&self) -> HashSet<Pubkey> {
            self.subscribed_programs.lock().clone()
        }

        /// Directly insert a subscription without going through subscribe().
        /// Useful for testing reconciliation scenarios.
        pub fn insert_subscription(&self, pubkey: Pubkey) {
            self.subscribed_pubkeys.lock().insert(pubkey);
        }
    }

    #[async_trait]
    impl ChainPubsubClient for ChainPubsubClientMock {
        fn take_updates(&self) -> mpsc::Receiver<SubscriptionUpdate> {
            // SAFETY: This can only be None if `take_updates` is called more
            // than once (double take). That would indicate a logic bug in the
            // calling code. Panicking here surfaces such a bug early and avoids
            // silently losing the updates stream.
            self.updates_rcvr.lock().take().expect(
                "ChainPubsubClientMock::take_updates called more than once",
            )
        }
        async fn subscribe(
            &self,
            pubkey: Pubkey,
            _retries: Option<usize>,
        ) -> RemoteAccountProviderResult<()> {
            if !*self.connected.lock() {
                return Err(
                    RemoteAccountProviderError::AccountSubscriptionsTaskFailed(
                        "mock: subscribe while disconnected".to_string(),
                    ),
                );
            }
            let mut subscribed_pubkeys = self.subscribed_pubkeys.lock();
            subscribed_pubkeys.insert(pubkey);
            Ok(())
        }

        async fn subscribe_program(
            &self,
            program_id: Pubkey,
        ) -> RemoteAccountProviderResult<()> {
            if !*self.connected.lock() {
                return Err(
                    RemoteAccountProviderError::AccountSubscriptionsTaskFailed(
                        "mock: subscribe_program while disconnected"
                            .to_string(),
                    ),
                );
            }
            let mut subscribed_programs = self.subscribed_programs.lock();
            subscribed_programs.insert(program_id);
            Ok(())
        }

        async fn unsubscribe(
            &self,
            pubkey: Pubkey,
        ) -> RemoteAccountProviderResult<()> {
            let mut subscribed_pubkeys = self.subscribed_pubkeys.lock();
            subscribed_pubkeys.remove(&pubkey);
            Ok(())
        }

        async fn shutdown(&self) -> RemoteAccountProviderResult<()> {
            Ok(())
        }

        async fn subscription_count(
            &self,
            exclude: Option<&[Pubkey]>,
        ) -> Option<(usize, usize)> {
            let pubkeys: Vec<Pubkey> = {
                let subs = self.subscribed_pubkeys.lock();
                subs.iter().cloned().collect()
            };
            let total = pubkeys.len();
            let exclude = exclude.unwrap_or_default();
            let filtered = pubkeys
                .iter()
                .filter(|pubkey| !exclude.contains(pubkey))
                .count();
            Some((total, filtered))
        }

        /// Returns the subscriptions of a client or the union of subscriptions
        /// if there are multiple clients.
        /// This means that if any client is subscribed to a pubkey, it will be
        /// included in the returned set even if other clients are not subscribed to it.
        fn subscriptions_union(&self) -> Option<HashSet<Pubkey>> {
            let subs = self.subscribed_pubkeys.lock();
            Some(subs.iter().copied().collect())
        }

        fn subs_immediately(&self) -> bool {
            true
        }

        fn id(&self) -> &str {
            "ChainPubsubClientMock"
        }
    }

    #[async_trait]
    impl ReconnectableClient for ChainPubsubClientMock {
        async fn try_reconnect(&self) -> RemoteAccountProviderResult<()> {
            if !*self.reconnectable.lock() {
                return Err(
                    RemoteAccountProviderError::AccountSubscriptionsTaskFailed(
                        "mock: reconnect failed".to_string(),
                    ),
                );
            }
            *self.connected.lock() = true;
            Ok(())
        }

        async fn resub_multiple(
            &self,
            pubkeys: HashSet<Pubkey>,
        ) -> RemoteAccountProviderResult<()> {
            // Simulate transient resubscription failures
            {
                let mut to_fail = self.pending_resubscribe_failures.lock();
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
                self.subscribe(pubkey, None).await?;
                // keep it small; tests shouldn't take long
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            Ok(())
        }
    }
}
