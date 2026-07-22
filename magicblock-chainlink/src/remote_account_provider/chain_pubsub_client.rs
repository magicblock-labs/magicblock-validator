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
    errors::{RemoteAccountProviderError, RemoteAccountProviderResult},
    pubsub_common::{ChainPubsubActorMessage, SubscriptionUpdate},
};

const MAX_RESUB_DELAY_MS: u64 = 800;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubscriptionReconciliationSnapshot {
    pub union: HashSet<Pubkey>,
    pub intersection: HashSet<Pubkey>,
}

/// Transport kind of a pubsub client. Websocket subscriptions are
/// per-account server-side resources, gRPC subscriptions are entries in a
/// shared stream filter and scale much further.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PubsubTransport {
    WebSocket,
    Grpc,
}

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

    /// Returns the subscriptions of a client or the union of subscriptions
    /// if there are multiple clients.
    /// This means that if any client is subscribed to a pubkey, it will be
    /// included in the returned set even if other clients are not subscribed to it.
    fn subscriptions_union(&self) -> HashSet<Pubkey>;

    /// Returns the intersection of subscriptions across all underlying
    /// clients. For a single client this is identical to [ChainPubsubClient::subscriptions_union].
    /// For an implementer with multiple clients it returns only the pubkeys
    /// that every client is subscribed to.
    fn subscriptions_intersection(&self) -> HashSet<Pubkey> {
        self.subscriptions_union()
    }

    /// Returns an atomic reconciliation snapshot. `None` means no live
    /// subscription client is available to inspect or repair.
    ///
    /// Default single-client implementations are always inspectable and derive
    /// both snapshot sets from one `subscriptions_union()` call. Implementers
    /// with connection availability state, such as `SubMuxClient`, must override
    /// this method and decide availability from the same state snapshot used to
    /// build the returned sets.
    fn subscription_reconciliation_snapshot(
        &self,
    ) -> Option<SubscriptionReconciliationSnapshot> {
        let union = self.subscriptions_union();
        Some(SubscriptionReconciliationSnapshot {
            intersection: union.clone(),
            union,
        })
    }

    /// Returns a reconciliation snapshot only when this client represents the
    /// requested transport. Multiplexers override this to aggregate their
    /// matching connected clients.
    fn subscription_reconciliation_snapshot_for_transport(
        &self,
        transport: PubsubTransport,
    ) -> Option<SubscriptionReconciliationSnapshot> {
        (self.transport() == transport)
            .then(|| self.subscription_reconciliation_snapshot())
            .flatten()
    }

    fn subs_immediately(&self) -> bool;

    fn id(&self) -> &str;

    fn transport(&self) -> PubsubTransport {
        PubsubTransport::WebSocket
    }

    /// Prefers gRPC-only coverage for `pubkey`.
    /// Multiplexing implementors drop websocket legs only after gRPC
    /// coverage is confirmed and err otherwise, leaving coverage untouched.
    async fn prefer_grpc_subscription(
        &self,
        pubkey: Pubkey,
    ) -> RemoteAccountProviderResult<()> {
        match self.transport() {
            PubsubTransport::Grpc => self.subscribe(pubkey, None).await,
            // Dropping a ws subscription is only safe behind a multiplexer
            // that confirmed gRPC coverage.
            PubsubTransport::WebSocket => {
                Err(RemoteAccountProviderError::AccountSubscriptionsTaskFailed(
                    format!(
                        "cannot prefer gRPC-only coverage for {pubkey} on a \
                         websocket-only client"
                    ),
                ))
            }
        }
    }
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
    /// The configured delay [Self::current_resub_delay_ms] is reset to
    /// after a resubscription pass completes without failures.
    initial_resub_delay_ms: u64,
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
        let initial_resub_delay_ms = resubscription_delay.as_millis() as u64;
        let current_resub_delay_ms =
            Arc::new(AtomicU64::new(initial_resub_delay_ms));
        Ok(Self {
            actor: Arc::new(actor),
            updates_rcvr: Arc::new(Mutex::new(Some(updates))),
            client_id,
            current_resub_delay_ms,
            initial_resub_delay_ms,
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

    fn subscriptions_union(&self) -> HashSet<Pubkey> {
        self.actor.subscriptions()
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
        // Failed keys are retried over just the remainder: erroring out
        // instead triggers a full reconnect that drains every subscription.
        const MAX_PASSES: usize = 3;
        // Only resubscribe what is still missing
        let subscribed = self.subscriptions_union();
        let had_active_subs = !subscribed.is_empty();
        let mut remaining: Vec<Pubkey> = pubkeys
            .into_iter()
            .filter(|pk| !subscribed.contains(pk))
            .collect();
        let total_subs = remaining.len();
        let mut fatal_err = None;

        for _ in 0..MAX_PASSES {
            if remaining.is_empty() {
                break;
            }
            let delay_ms = self.current_resub_delay_ms.load(Ordering::SeqCst);
            let delay = Duration::from_millis(delay_ms);
            let mut failed = Vec::new();
            let mut last_err = None;
            for (idx, pubkey) in remaining.iter().enumerate() {
                match self
                    .subscribe(*pubkey, Some(RESUB_MULTIPLE_RETRY_PER_PUBKEY))
                    .await
                {
                    Ok(()) => {
                        // Pace successful subscribes only; failures
                        // already backed off internally
                        if idx + 1 < remaining.len() {
                            tokio::time::sleep(delay).await;
                        }
                    }
                    Err(err) => {
                        debug!(
                            error = ?err,
                            pubkey = %pubkey,
                            "Re-subscription failed for pubkey",
                        );
                        failed.push(*pubkey);
                        last_err = Some(err);
                    }
                }
            }
            if !failed.is_empty() {
                // Exponentially back off on resubscription attempts, so the next time we
                // reconnect and try to resubscribe, we wait longer in between each subscription
                // in order to avoid overwhelming the RPC with requests
                let new_delay =
                    delay_ms.saturating_mul(2).min(MAX_RESUB_DELAY_MS);
                self.current_resub_delay_ms
                    .store(new_delay, Ordering::SeqCst);
            }
            let no_progress = failed.len() == remaining.len();
            remaining = failed;
            if no_progress {
                // Further passes won't do better. Error out (triggering a
                // full reconnect) only when there is nothing to lose:
                // nothing restored here and no pre-existing subscriptions.
                if remaining.len() == total_subs && !had_active_subs {
                    fatal_err = last_err;
                }
                break;
            }
        }

        metrics::set_pubsub_client_resubscribed_count(
            &self.client_id,
            total_subs - remaining.len(),
        );
        if let Some(err) = fatal_err {
            warn!(total_subs, "Re-subscription made no progress");
            return Err(err);
        }
        if remaining.is_empty() {
            // Clean pass: restore the configured pacing
            self.current_resub_delay_ms
                .store(self.initial_resub_delay_ms, Ordering::SeqCst);
        } else {
            // Leftovers are repaired by the subscription reconciler
            warn!(
                total_subs,
                failed = remaining.len(),
                "Re-subscription completed with failures, \
                 leaving repair to the reconciler",
            );
        }
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
    use std::{
        collections::HashSet,
        sync::atomic::{AtomicU16, AtomicU64, Ordering as AtomicOrdering},
        time::Duration,
    };

    use parking_lot::Mutex;
    use solana_account::Account;
    use solana_account_decoder::{encode_ui_account, UiAccountEncoding};
    use solana_program::clock::Slot;
    use solana_rpc_client_api::response::{
        Response as RpcResponse, RpcResponseContext,
    };
    use tokio::sync::Notify;
    use tracing::*;

    use super::*;
    use crate::remote_account_provider::{
        pubsub_common::SubscriptionSource, RemoteAccountProviderError,
        RemoteAccountProviderResult,
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
        pending_unsubscribe_failures: Arc<Mutex<usize>>,
        pending_program_subscribe_failures: Arc<Mutex<usize>>,
        pending_silent_subscribe_noops: Arc<Mutex<usize>>,
        reconnectable: Arc<Mutex<bool>>,
        subscribe_blocked: Arc<Mutex<bool>>,
        subscribe_pause_after_insert: Arc<Mutex<bool>>,
        subscribe_insertions: Arc<AtomicU64>,
        subscribe_insert_notify: Arc<Notify>,
        subscribe_continue_notify: Arc<Notify>,
        subscribe_attempts: Arc<AtomicU64>,
        program_subscribe_attempts: Arc<AtomicU64>,
        subscribe_notify: Arc<Notify>,
        client_id: String,
        transport: Arc<Mutex<PubsubTransport>>,
        prefer_grpc_calls: Arc<Mutex<Vec<Pubkey>>>,
    }

    impl ChainPubsubClientMock {
        pub fn new(
            updates_sndr: mpsc::Sender<SubscriptionUpdate>,
            updates_rcvr: mpsc::Receiver<SubscriptionUpdate>,
        ) -> Self {
            static CLIENT_ID: AtomicU16 = AtomicU16::new(0);

            Self {
                updates_sndr,
                updates_rcvr: Arc::new(Mutex::new(Some(updates_rcvr))),
                subscribed_pubkeys: Arc::new(Mutex::new(HashSet::new())),
                subscribed_programs: Arc::new(Mutex::new(HashSet::new())),
                subscription_count_at_disconnect: Arc::new(Mutex::new(0)),
                connected: Arc::new(Mutex::new(true)),
                pending_resubscribe_failures: Arc::new(Mutex::new(0)),
                pending_unsubscribe_failures: Arc::new(Mutex::new(0)),
                pending_program_subscribe_failures: Arc::new(Mutex::new(0)),
                pending_silent_subscribe_noops: Arc::new(Mutex::new(0)),
                reconnectable: Arc::new(Mutex::new(true)),
                subscribe_blocked: Arc::new(Mutex::new(false)),
                subscribe_pause_after_insert: Arc::new(Mutex::new(false)),
                subscribe_insertions: Arc::new(AtomicU64::new(0)),
                subscribe_insert_notify: Arc::new(Notify::new()),
                subscribe_continue_notify: Arc::new(Notify::new()),
                subscribe_attempts: Arc::new(AtomicU64::new(0)),
                program_subscribe_attempts: Arc::new(AtomicU64::new(0)),
                subscribe_notify: Arc::new(Notify::new()),
                client_id: format!(
                    "mock:{}",
                    CLIENT_ID.fetch_add(1, AtomicOrdering::SeqCst)
                ),
                transport: Arc::new(Mutex::new(PubsubTransport::WebSocket)),
                prefer_grpc_calls: Arc::new(Mutex::new(Vec::new())),
            }
        }

        pub fn set_transport(&self, transport: PubsubTransport) {
            *self.transport.lock() = transport;
        }

        /// Pubkeys for which `prefer_grpc_subscription` was invoked.
        pub fn prefer_grpc_calls(&self) -> Vec<Pubkey> {
            self.prefer_grpc_calls.lock().clone()
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

        pub fn fail_next_unsubscriptions(&self, n: usize) {
            *self.pending_unsubscribe_failures.lock() = n;
        }

        pub fn fail_next_program_subscriptions(&self, n: usize) {
            *self.pending_program_subscribe_failures.lock() = n;
        }

        /// Next N subscribe() calls return Ok without registering the
        /// subscription.
        pub fn silently_noop_next_subscriptions(&self, n: usize) {
            *self.pending_silent_subscribe_noops.lock() = n;
        }

        pub fn program_subscribe_attempts(&self) -> u64 {
            self.program_subscribe_attempts.load(AtomicOrdering::SeqCst)
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
            let update = SubscriptionUpdate::from_rpc_response(
                pubkey,
                rpc_response,
                SubscriptionSource::Account,
            );
            self.send(update).await;
        }

        pub fn disable_reconnect(&self) {
            *self.reconnectable.lock() = false;
        }

        pub fn enable_reconnect(&self) {
            *self.reconnectable.lock() = true;
        }

        /// Block the `subscribe()` method from completing.
        pub fn block_subscribe(&self) {
            *self.subscribe_blocked.lock() = true;
        }

        /// Release the `subscribe()` method to complete.
        pub fn release_subscribe(&self) {
            *self.subscribe_blocked.lock() = false;
            self.subscribe_notify.notify_waiters();
        }

        /// Pause subscribe() after it has inserted the pubkey into the mock
        /// pubsub set but before subscribe() returns to the caller.
        pub fn pause_after_subscribe_insert(&self) {
            *self.subscribe_pause_after_insert.lock() = true;
        }

        /// Resume subscribe() calls paused by pause_after_subscribe_insert().
        pub fn resume_after_subscribe_insert(&self) {
            *self.subscribe_pause_after_insert.lock() = false;
            self.subscribe_continue_notify.notify_waiters();
        }

        pub fn subscribe_insertions(&self) -> u64 {
            self.subscribe_insertions.load(AtomicOrdering::SeqCst)
        }

        pub async fn wait_for_subscribe_insertions(&self, min_insertions: u64) {
            loop {
                let notified = self.subscribe_insert_notify.notified();
                tokio::pin!(notified);
                let current =
                    self.subscribe_insertions.load(AtomicOrdering::SeqCst);
                if current >= min_insertions {
                    break;
                }
                notified.await;
            }
        }

        /// Wait until at least `min_attempts` subscribe attempts have been made.
        /// Used for testing to ensure subscribes are being called.
        pub async fn wait_for_subscribe_attempts(&self, min_attempts: u64) {
            loop {
                // Register the waiter BEFORE checking the condition to avoid
                // lost notifications: if we checked first and a notification
                // arrived between the check and the await, we would miss it.
                let notified = self.subscribe_notify.notified();
                tokio::pin!(notified);
                let current =
                    self.subscribe_attempts.load(AtomicOrdering::SeqCst);
                if current >= min_attempts {
                    break;
                }
                notified.await;
            }
        }

        pub fn subscribe_attempts(&self) -> u64 {
            self.subscribe_attempts.load(AtomicOrdering::SeqCst)
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

        /// Directly remove a subscription without going through unsubscribe().
        /// Useful for testing already-missing cleanup scenarios.
        pub fn remove_subscription(&self, pubkey: &Pubkey) {
            self.subscribed_pubkeys.lock().remove(pubkey);
        }
    }

    #[async_trait]
    impl ChainPubsubClient for ChainPubsubClientMock {
        /// Records the call, then mirrors the trait default: subscribe on a
        /// gRPC client, error on a websocket-only client.
        async fn prefer_grpc_subscription(
            &self,
            pubkey: Pubkey,
        ) -> RemoteAccountProviderResult<()> {
            self.prefer_grpc_calls.lock().push(pubkey);
            match self.transport() {
                PubsubTransport::Grpc => self.subscribe(pubkey, None).await,
                PubsubTransport::WebSocket => Err(
                    RemoteAccountProviderError::AccountSubscriptionsTaskFailed(
                        format!(
                            "cannot prefer gRPC-only coverage for {pubkey} on \
                             a websocket-only client"
                        ),
                    ),
                ),
            }
        }

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
            // Increment attempt counter and notify waiters
            self.subscribe_attempts.fetch_add(1, AtomicOrdering::SeqCst);
            self.subscribe_notify.notify_waiters();

            // Wait until subscribe is released if it's blocked.
            // Register the waiter BEFORE re-checking the condition so that a
            // `release_subscribe()` notification fired between the check and
            // the await is not lost.
            loop {
                let notified = self.subscribe_notify.notified();
                tokio::pin!(notified);
                if !*self.subscribe_blocked.lock() {
                    break;
                }
                notified.await;
            }

            if !*self.connected.lock() {
                return Err(
                    RemoteAccountProviderError::AccountSubscriptionsTaskFailed(
                        "mock: subscribe while disconnected".to_string(),
                    ),
                );
            }
            {
                let mut to_noop = self.pending_silent_subscribe_noops.lock();
                if *to_noop > 0 {
                    *to_noop -= 1;
                    return Ok(());
                }
            }
            {
                let mut subscribed_pubkeys = self.subscribed_pubkeys.lock();
                subscribed_pubkeys.insert(pubkey);
            }
            self.subscribe_insertions
                .fetch_add(1, AtomicOrdering::SeqCst);
            self.subscribe_insert_notify.notify_waiters();

            loop {
                let notified = self.subscribe_continue_notify.notified();
                tokio::pin!(notified);
                if !*self.subscribe_pause_after_insert.lock() {
                    break;
                }
                notified.await;
            }

            Ok(())
        }

        async fn subscribe_program(
            &self,
            program_id: Pubkey,
        ) -> RemoteAccountProviderResult<()> {
            self.program_subscribe_attempts
                .fetch_add(1, AtomicOrdering::SeqCst);

            {
                let mut to_fail =
                    self.pending_program_subscribe_failures.lock();
                if *to_fail > 0 {
                    *to_fail -= 1;
                    return Err(
                        RemoteAccountProviderError::AccountSubscriptionsTaskFailed(
                            "mock: forced program subscribe failure"
                                .to_string(),
                        ),
                    );
                }
            }

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
            eprintln!(
                "UNSUB_CALL {} at:\n{}",
                pubkey,
                std::backtrace::Backtrace::force_capture()
            );
            {
                let mut to_fail = self.pending_unsubscribe_failures.lock();
                if *to_fail > 0 {
                    *to_fail -= 1;
                    return Err(
                        RemoteAccountProviderError::AccountSubscriptionsTaskFailed(
                            "mock: forced unsubscribe failure".to_string(),
                        ),
                    );
                }
            }

            let mut subscribed_pubkeys = self.subscribed_pubkeys.lock();
            if subscribed_pubkeys.remove(&pubkey) {
                Ok(())
            } else {
                Err(
                    RemoteAccountProviderError::AccountSubscriptionDoesNotExist(
                        pubkey.to_string(),
                    ),
                )
            }
        }

        async fn shutdown(&self) -> RemoteAccountProviderResult<()> {
            Ok(())
        }

        fn subscriptions_union(&self) -> HashSet<Pubkey> {
            let subs = self.subscribed_pubkeys.lock();
            subs.iter().copied().collect()
        }

        fn subs_immediately(&self) -> bool {
            true
        }

        fn id(&self) -> &str {
            &self.client_id
        }

        fn transport(&self) -> PubsubTransport {
            *self.transport.lock()
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
