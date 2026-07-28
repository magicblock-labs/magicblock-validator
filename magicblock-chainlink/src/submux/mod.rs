#[cfg(test)]
use std::sync::atomic::AtomicU64;
use std::{
    cmp,
    collections::{HashMap, HashSet, VecDeque},
    hash::{Hash, Hasher},
    sync::{
        atomic::{AtomicBool, AtomicU16, Ordering},
        Arc, Mutex, MutexGuard,
    },
    time::{Duration, Instant},
};

use async_trait::async_trait;
use magicblock_metrics::metrics;
use solana_pubkey::Pubkey;
use solana_sdk_ids::sysvar::clock;
use tokio::sync::{mpsc, Notify};
use tokio_util::sync::CancellationToken;
use tracing::*;

use crate::remote_account_provider::{
    chain_pubsub_client::{
        AccountSubscriptionPublicationOutcome,
        AccountSubscriptionPublicationPolicy,
        AccountSubscriptionPublicationToken, ChainPubsubClient,
        PubsubTransport, ReconnectableClient,
        SubscriptionReconciliationSnapshot,
    },
    errors::{RemoteAccountProviderError, RemoteAccountProviderResult},
    pubsub_common::SubscriptionUpdate,
    SubscriptionKeyGuard, SubscriptionKeyLocks,
};

const SUBMUX_OUT_CHANNEL_SIZE: usize = 5_000;
const DEDUP_WINDOW_MILLIS: u64 = 2_000;
const DEBOUNCE_INTERVAL_MILLIS: u64 = 2_000;

type SubscriptionOperationLocks = SubscriptionKeyLocks;

mod debounce_state;
pub use self::debounce_state::DebounceState;

mod subscription_task;
pub use self::subscription_task::AccountSubscriptionTask;
use self::subscription_task::{SUBSCRIBE_TIMEOUT, UNSUBSCRIBE_TIMEOUT};

mod subscribed_accounts_tracker;
pub use self::subscribed_accounts_tracker::SubscribedAccountsTracker;

#[derive(Debug, Clone, Copy, Default)]
pub struct DebounceConfig {
    /// The deduplication window in milliseconds. If None, defaults to
    /// DEDUP_WINDOW_MILLIS.
    pub dedupe_window_millis: Option<u64>,
    /// The debounce interval in milliseconds. If None, defaults to
    /// DEBOUNCE_INTERVAL_MILLIS.
    pub interval_millis: Option<u64>,
    /// The detection window in milliseconds. If None, defaults to 5x the
    /// selected interval.
    pub detection_window_millis: Option<u64>,
}

enum SubscriptionSetRollback {
    Insert,
    Remove,
}

struct SubscriptionSetGuard<'a> {
    subscriptions: &'a Mutex<HashSet<Pubkey>>,
    pubkey: Pubkey,
    rollback_on_drop: Option<SubscriptionSetRollback>,
}

struct OwnedSubscriptionSetGuard {
    subscriptions: Arc<Mutex<HashSet<Pubkey>>>,
    pubkey: Pubkey,
    rollback_on_drop: Option<SubscriptionSetRollback>,
}

#[derive(Debug, Clone, Copy)]
enum PublicationDirtyKey {
    Account(Pubkey),
    Program(Pubkey),
}

struct PendingProviderPublication {
    token: AccountSubscriptionPublicationToken,
    policy: AccountSubscriptionPublicationPolicy,
}

struct AttachingClientPublication {
    attempt: u64,
    transport: PubsubTransport,
    dirty_accounts: HashSet<Pubkey>,
    dirty_programs: HashSet<Pubkey>,
    blockers: HashSet<u64>,
    refresh_account_policy: bool,
    serialize_account_refresh: bool,
    notify: Arc<Notify>,
}

#[derive(Default)]
struct ClientPublicationState {
    next_generation: u64,
    attaching_clients: HashMap<usize, AttachingClientPublication>,
    active_operations: HashMap<u64, PublicationDirtyKey>,
    provider_publications: HashMap<Pubkey, Vec<PendingProviderPublication>>,
}

impl ClientPublicationState {
    fn next_generation(&mut self) -> u64 {
        self.next_generation = self.next_generation.wrapping_add(1);
        self.next_generation
    }

    fn mark_dirty(&mut self, dirty: PublicationDirtyKey) {
        for attach in self.attaching_clients.values_mut() {
            match dirty {
                PublicationDirtyKey::Account(pubkey) => {
                    attach.dirty_accounts.insert(pubkey);
                }
                PublicationDirtyKey::Program(program_id) => {
                    attach.dirty_programs.insert(program_id);
                }
            }
            attach.notify.notify_one();
        }
    }

    fn finish_operation(&mut self, generation: u64) {
        let Some(dirty) = self.active_operations.remove(&generation) else {
            return;
        };
        for attach in self.attaching_clients.values_mut() {
            attach.blockers.remove(&generation);
        }
        self.mark_dirty(dirty);
    }

    fn pending_policy(
        &self,
        pubkey: &Pubkey,
    ) -> Option<AccountSubscriptionPublicationPolicy> {
        self.provider_publications
            .get(pubkey)
            .and_then(|publications| publications.last())
            .map(|publication| publication.policy)
    }
}

struct PublicationOperationGuard {
    state: Arc<Mutex<ClientPublicationState>>,
    generation: Option<u64>,
}

impl Drop for PublicationOperationGuard {
    fn drop(&mut self) {
        let Some(generation) = self.generation.take() else {
            return;
        };
        self.state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .finish_operation(generation);
    }
}

struct AttachingClientGuard {
    state: Arc<Mutex<ClientPublicationState>>,
    client_key: usize,
    attempt: u64,
    notify: Arc<Notify>,
    armed: bool,
}

enum AttachingClientWork {
    RefreshAccountPolicy { serialize: bool },
    Accounts(Vec<Pubkey>),
    Programs(Vec<Pubkey>),
    Wait,
    Published { was_disconnected: bool },
    Stale,
}

impl AttachingClientGuard {
    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for AttachingClientGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if state
            .attaching_clients
            .get(&self.client_key)
            .is_some_and(|attach| attach.attempt == self.attempt)
        {
            state.attaching_clients.remove(&self.client_key);
        }
    }
}

impl SubscriptionSetGuard<'_> {
    fn commit(&mut self) {
        self.rollback_on_drop = None;
    }
}

impl Drop for SubscriptionSetGuard<'_> {
    fn drop(&mut self) {
        let Some(rollback) = self.rollback_on_drop.take() else {
            return;
        };
        let mut subscriptions = self
            .subscriptions
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        match rollback {
            SubscriptionSetRollback::Insert => {
                subscriptions.insert(self.pubkey);
            }
            SubscriptionSetRollback::Remove => {
                subscriptions.remove(&self.pubkey);
            }
        }
    }
}

impl OwnedSubscriptionSetGuard {
    fn commit(&mut self) {
        self.rollback_on_drop = None;
    }
}

impl Drop for OwnedSubscriptionSetGuard {
    fn drop(&mut self) {
        let Some(rollback) = self.rollback_on_drop.take() else {
            return;
        };
        let mut subscriptions = self
            .subscriptions
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        match rollback {
            SubscriptionSetRollback::Insert => {
                subscriptions.insert(self.pubkey);
            }
            SubscriptionSetRollback::Remove => {
                subscriptions.remove(&self.pubkey);
            }
        }
    }
}

/// SubMuxClient
///
/// Multi-node pub/sub subscription multiplexer that:
/// - fans out subscribe/unsubscribe to all inner clients
/// - fans in their updates into a single output stream
///
/// Deduplication:
///
/// - Identical updates (same pubkey and slot) coming from different
///   inner clients are forwarded only once within a configurable
///   dedup_window.
///
/// Debounce strategy:
///
/// - Goal: When an account starts producing updates too frequently,
///   coalesce them and forward at most one update per
///   `debounce_interval`, always forwarding the most recent payload.
///
/// - Definitions:
///   - allowed_count (N): integer computed as
///     [Self::debounce_detection_window] / [Self::debounce_interval].
///     This is the number of most-recent arrivals we inspect to decide
///     on enabling debouncing.
///
/// - Entering debounce mode (Enabled):
///   1) On every incoming update, we prune the per-account arrival
///      timestamps to only keep those within the
///      debounce_detection_window, then push the current arrival time.
///   2) If we have at least N arrivals and the last N inter-arrival
///      deltas are each <= debounce_interval (i.e., the stream is at
///      least one update per interval or faster), we transition the
///      account to DebounceState::Enabled immediately. This satisfies
///      the rule: "we enter it only after a certain number of updates
///      were too frequent" (that number is N).
///
/// - Exiting debounce mode (Disabled):
///   - On every new arrival we re-evaluate. If the above condition is
///     not met (for example, because the most recent gap is >
///     debounce_interval, or because pruning dropped the history below
///     N), we immediately transition back to
///     DebounceState::Disabled. This satisfies the rule: "we exit it
///     immediately when an update is above the min interval". The very
///     update that triggers exit is forwarded right away since we are no
///     longer debouncing.
///
/// - Forwarding while debounced:
///   - When in Enabled state, if an arrival occurs at or after the
///     `next_allowed_forward` timestamp, it is forwarded immediately and
///     `next_allowed_forward` is advanced by `debounce_interval`.
///   - Otherwise, we store/replace a single pending update for that
///     account. A global flusher task runs periodically (at about a
///     quarter of the debounce interval) and forwards any pending update
///     whose `next_allowed_forward` has arrived. This avoids per-update
///     timer tasks at the cost of a bounded (<= ~interval/4) delay in
///     the corner case where bursts stop just before eligibility.
///
/// - Always latest payload:
///   - While waiting for eligibility in Enabled state, only the latest
///     observed update is kept as pending so that the consumer receives
///     the freshest state when the interval elapses.
pub struct SubMuxClient<T>
where
    T: ChainPubsubClient + ReconnectableClient,
{
    /// Underlying pubsub clients this mux controls and forwards to/from.
    clients: Arc<Mutex<Vec<Arc<T>>>>,
    /// Aggregated outgoing channel used by forwarder tasks to deliver
    /// subscription updates to the consumer of this SubMuxClient.
    out_tx: mpsc::Sender<SubscriptionUpdate>,
    /// Receiver end for the aggregated updates. Taken exactly once via
    /// take_updates(); wrapped in Arc<Mutex<Option<...>>> so the struct
    /// remains Clone and the receiver can be moved out safely.
    out_rx: Arc<Mutex<Option<mpsc::Receiver<SubscriptionUpdate>>>>,
    /// Deduplication cache keyed by (pubkey, slot) storing the last time
    /// we forwarded such an update. Prevents forwarding identical updates
    /// seen from multiple inner clients within dedup_window.
    dedup_cache: Arc<Mutex<HashMap<(Pubkey, u64), Instant>>>,
    /// Time window during which identical updates are suppressed.
    dedup_window: Duration,
    /// When debouncing is enabled for a pubkey, at most one update per
    /// this interval will be forwarded (the latest pending one).
    debounce_interval: Duration,
    /// Sliding time window used to detect high-frequency streams that
    /// should be debounced and to later disable debounce when traffic
    /// drops below the rate again.
    debounce_detection_window: Duration,
    /// Per-account debounce state tracking (enabled/disabled, arrivals,
    /// next-allowed-forward timestamp and pending update).
    debounce_states: Arc<Mutex<HashMap<Pubkey, DebounceState>>>,
    /// Accounts that should never be debounced, namely the clock sysvar account
    /// which we use to track the latest remote slot.
    never_debounce: HashSet<Pubkey>,
    /// Map of program account subscriptions we are holding inside the pubsub clients
    program_subs: Arc<Mutex<HashSet<Pubkey>>>,
    /// Program subscriptions that may have partial transport coverage after
    /// a failed quorum and should be retried on the next explicit acquire.
    unconfirmed_program_subs: Arc<Mutex<HashSet<Pubkey>>>,
    /// Accounts whose desired coverage excludes websocket clients while at
    /// least one gRPC client is available.
    grpc_only_subscriptions: Arc<Mutex<HashSet<Pubkey>>>,
    /// Account removals that must override a still-stale tracker snapshot
    /// while provider state is being committed. Reconnect and attach paths
    /// exclude these keys; a later successful subscribe clears the tombstone.
    unsubscribed_accounts: Arc<Mutex<HashSet<Pubkey>>>,
    /// Serializes account-subscription transport operations by pubkey. The
    /// operation's spawned fanout retains the guard after quorum completion,
    /// so a later subscribe cannot race a still-running unsubscribe leg (or
    /// vice versa).
    subscription_operation_locks: SubscriptionOperationLocks,
    /// Short-held publication journal for attaching/reconnecting clients.
    /// Network I/O never runs while this mutex is held.
    client_publication_state: Arc<Mutex<ClientPublicationState>>,
    #[cfg(test)]
    grpc_preference_completions: Arc<AtomicU64>,
    /// Client handles currently considered connected by the mux.
    connected_client_ids: Arc<Mutex<HashSet<usize>>>,
    /// Number of currently connected pubsub clients.
    connected_clients: Arc<AtomicU16>,
    /// Number of currently connected clients that activate subscriptions immediately when
    /// requested.
    connected_clients_subscribing_immediately: Arc<AtomicU16>,
    /// Whether take_updates() has started the per-client forwarders.
    forwarders_started: Arc<AtomicBool>,
    /// Token cancelled on drop to stop background tasks
    /// (dedup pruner, debounce flusher).
    shutdown_token: CancellationToken,
    /// Only the original handle owns shutdown. Clones are used by detached
    /// helpers and should not keep the mux alive.
    cancel_on_drop: bool,
}

// Parameters for the long-running forwarder loop, grouped to avoid
// clippy::too_many_arguments and to keep spawn sites concise.
struct ForwarderParams {
    tx: mpsc::Sender<SubscriptionUpdate>,
    cache: Arc<Mutex<HashMap<(Pubkey, u64), Instant>>>,
    debounce_states: Arc<Mutex<HashMap<Pubkey, DebounceState>>>,
    window: Duration,
    debounce_interval: Duration,
    detection_window: Duration,
    allowed_count: usize,
}

impl<T> SubMuxClient<T>
where
    T: ChainPubsubClient + ReconnectableClient,
{
    pub fn new<U: SubscribedAccountsTracker>(
        clients: Vec<(Arc<T>, mpsc::Receiver<()>)>,
        subscribed_accounts_tracker: Arc<U>,
        dedupe_window_millis: Option<u64>,
    ) -> Self {
        Self::new_with_debounce(
            clients,
            subscribed_accounts_tracker,
            DebounceConfig {
                dedupe_window_millis,
                ..DebounceConfig::default()
            },
        )
    }

    pub fn new_with_debounce<U: SubscribedAccountsTracker>(
        clients: Vec<(Arc<T>, mpsc::Receiver<()>)>,
        subscribed_accounts_tracker: Arc<U>,
        config: DebounceConfig,
    ) -> Self {
        Self::new_with_config(clients, subscribed_accounts_tracker, config)
    }

    pub fn new_with_config<U: SubscribedAccountsTracker>(
        clients: Vec<(Arc<T>, mpsc::Receiver<()>)>,
        subscribed_accounts_tracker: Arc<U>,
        config: DebounceConfig,
    ) -> Self {
        let (out_tx, out_rx) = mpsc::channel(SUBMUX_OUT_CHANNEL_SIZE);
        let dedup_cache = Arc::new(Mutex::new(HashMap::new()));
        let debounce_states = Arc::new(Mutex::new(HashMap::new()));
        let dedup_window = Duration::from_millis(
            config.dedupe_window_millis.unwrap_or(DEDUP_WINDOW_MILLIS),
        );
        let interval_ms =
            config.interval_millis.unwrap_or(DEBOUNCE_INTERVAL_MILLIS);
        let detection_ms = config
            .detection_window_millis
            .unwrap_or(interval_ms.saturating_mul(5));
        let debounce_interval = Duration::from_millis(interval_ms);
        let debounce_detection_window = Duration::from_millis(detection_ms);

        let never_debounce: HashSet<Pubkey> =
            vec![clock::ID].into_iter().collect();

        let program_subs: Arc<Mutex<HashSet<Pubkey>>> = Default::default();
        let unconfirmed_program_subs: Arc<Mutex<HashSet<Pubkey>>> =
            Default::default();
        let grpc_only_subscriptions: Arc<Mutex<HashSet<Pubkey>>> =
            Default::default();
        let unsubscribed_accounts: Arc<Mutex<HashSet<Pubkey>>> =
            Default::default();
        let subscription_operation_locks: SubscriptionOperationLocks =
            Default::default();
        let client_publication_state: Arc<Mutex<ClientPublicationState>> =
            Default::default();
        let connected_client_ids: Arc<Mutex<HashSet<usize>>> =
            Arc::new(Mutex::new(
                clients
                    .iter()
                    .map(|(client, _)| Self::client_key(client))
                    .collect(),
            ));

        // Initialize the tracking of the number of connected clients and their uptime.
        // We assume all clients are connected at startup.
        let connected_clients = {
            let n = clients.len();
            metrics::set_connected_pubsub_clients_count(n);
            Arc::new(AtomicU16::new(n as u16))
        };

        let connected_clients_subscribing_immediately = {
            let n = clients
                .iter()
                .filter(|(client, _)| client.subs_immediately())
                .count();
            metrics::set_connected_direct_pubsub_clients_count(n);
            Arc::new(AtomicU16::new(n.try_into().unwrap_or(u16::MAX)))
        };
        for (client, _) in &clients {
            metrics::set_pubsub_client_uptime(client.id(), true);
            if let Some(delay_ms) = client.current_resub_delay_ms() {
                metrics::set_pubsub_client_resubscribe_delay(
                    client.id(),
                    delay_ms,
                );
            }
        }

        let clients_only = Arc::new(Mutex::new(
            clients
                .iter()
                .map(|(client, _)| client.clone())
                .collect::<Vec<_>>(),
        ));

        Self::spawn_reconnectors(
            clients,
            clients_only.clone(),
            subscribed_accounts_tracker,
            program_subs.clone(),
            grpc_only_subscriptions.clone(),
            unsubscribed_accounts.clone(),
            subscription_operation_locks.clone(),
            client_publication_state.clone(),
            never_debounce.clone(),
            connected_client_ids.clone(),
            connected_clients.clone(),
            connected_clients_subscribing_immediately.clone(),
        );

        let shutdown_token = CancellationToken::new();
        let me = Self {
            clients: clients_only,
            out_tx,
            out_rx: Arc::new(Mutex::new(Some(out_rx))),
            dedup_cache: dedup_cache.clone(),
            dedup_window,
            debounce_interval,
            debounce_detection_window,
            debounce_states: debounce_states.clone(),
            never_debounce,
            program_subs,
            unconfirmed_program_subs,
            grpc_only_subscriptions,
            unsubscribed_accounts,
            subscription_operation_locks,
            client_publication_state,
            #[cfg(test)]
            grpc_preference_completions: Arc::new(AtomicU64::new(0)),
            connected_client_ids,
            connected_clients,
            connected_clients_subscribing_immediately,
            forwarders_started: Arc::new(AtomicBool::new(false)),
            shutdown_token,
            cancel_on_drop: true,
        };

        // Spawn background tasks
        me.spawn_dedup_pruner();
        me.spawn_debounce_flusher();
        me
    }

    /// Token cancelled when the owning mux shuts down; ties background
    /// tasks to this mux's lifetime.
    pub(crate) fn shutdown_token(&self) -> CancellationToken {
        self.shutdown_token.clone()
    }

    // -----------------
    // Reconnection
    // -----------------
    #[allow(clippy::too_many_arguments)]
    fn spawn_reconnectors<U: SubscribedAccountsTracker>(
        clients: Vec<(Arc<T>, mpsc::Receiver<()>)>,
        all_clients: Arc<Mutex<Vec<Arc<T>>>>,
        subscribed_accounts_tracker: Arc<U>,
        program_subs: Arc<Mutex<HashSet<Pubkey>>>,
        grpc_only_subscriptions: Arc<Mutex<HashSet<Pubkey>>>,
        unsubscribed_accounts: Arc<Mutex<HashSet<Pubkey>>>,
        subscription_operation_locks: SubscriptionOperationLocks,
        client_publication_state: Arc<Mutex<ClientPublicationState>>,
        never_debounce: HashSet<Pubkey>,
        connected_client_ids: Arc<Mutex<HashSet<usize>>>,
        connected_clients: Arc<AtomicU16>,
        connected_clients_subscribing_immediately: Arc<AtomicU16>,
    ) {
        for (client, mut abort_rx) in clients.into_iter() {
            let all_clients = all_clients.clone();
            let subscribed_accounts_tracker =
                subscribed_accounts_tracker.clone();
            let program_subs = program_subs.clone();
            let grpc_only_subscriptions = grpc_only_subscriptions.clone();
            let unsubscribed_accounts = unsubscribed_accounts.clone();
            let subscription_operation_locks =
                subscription_operation_locks.clone();
            let client_publication_state = client_publication_state.clone();
            let never_debounce = never_debounce.clone();
            let connected_client_ids = connected_client_ids.clone();
            let connected_clients = connected_clients.clone();
            let connected_clients_subscribing_immediately =
                connected_clients_subscribing_immediately.clone();
            tokio::spawn(async move {
                while (abort_rx.recv().await).is_some() {
                    // Drain any duplicate abort signals to coalesce reconnect attempts
                    while abort_rx.try_recv().is_ok() {}

                    debug!(client_id = %client.id(), "Reconnecter received abort signal");

                    // Update connection related metrics
                    let was_connected = {
                        let mut publication = client_publication_state
                            .lock()
                            .unwrap_or_else(|poison| poison.into_inner());
                        let mut connected_ids = Self::connected_client_ids_lock(
                            &connected_client_ids,
                        );
                        let removed =
                            connected_ids.remove(&Self::client_key(&client));
                        if removed
                            && client.transport() == PubsubTransport::Grpc
                        {
                            Self::mark_websocket_policy_refresh(
                                &mut publication,
                            );
                        }
                        removed
                    };
                    if was_connected
                        && client.transport() == PubsubTransport::Grpc
                    {
                        Self::spawn_connected_websocket_policy_refresh(
                            all_clients.clone(),
                            subscribed_accounts_tracker.clone(),
                            program_subs.clone(),
                            grpc_only_subscriptions.clone(),
                            unsubscribed_accounts.clone(),
                            subscription_operation_locks.clone(),
                            client_publication_state.clone(),
                            connected_client_ids.clone(),
                            never_debounce.clone(),
                        );
                    }
                    if was_connected {
                        connected_clients.fetch_sub(1, Ordering::SeqCst);
                        metrics::set_connected_pubsub_clients_count(
                            connected_clients.load(Ordering::SeqCst) as usize,
                        );
                        if client.subs_immediately() {
                            let previous =
                                connected_clients_subscribing_immediately
                                    .fetch_sub(1, Ordering::SeqCst);
                            let current = previous.saturating_sub(1);
                            metrics::set_connected_direct_pubsub_clients_count(
                                current as usize,
                            );
                            debug!(
                                client_id = %client.id(),
                                previous,
                                current,
                                "Connected clients subscribing immediately"
                            );
                        }
                    }
                    metrics::set_pubsub_client_uptime(client.id(), false);

                    Self::reconnect_client_with_backoff(
                        client.clone(),
                        all_clients.clone(),
                        subscribed_accounts_tracker.clone(),
                        program_subs.clone(),
                        grpc_only_subscriptions.clone(),
                        unsubscribed_accounts.clone(),
                        subscription_operation_locks.clone(),
                        client_publication_state.clone(),
                        never_debounce.clone(),
                        connected_client_ids.clone(),
                        connected_clients.clone(),
                        connected_clients_subscribing_immediately.clone(),
                    )
                    .await;
                }
            });
        }
    }

    fn clients_snapshot(&self) -> Vec<Arc<T>> {
        self.clients_lock().clone()
    }

    fn client_key(client: &Arc<T>) -> usize {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        client.id().hash(&mut hasher);
        hasher.finish() as usize
    }

    fn connected_client_ids_lock(
        connected_client_ids: &Arc<Mutex<HashSet<usize>>>,
    ) -> MutexGuard<'_, HashSet<usize>> {
        match connected_client_ids.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    fn connected_clients_snapshot(&self) -> Vec<Arc<T>> {
        let _publication = self
            .client_publication_state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let clients = self.clients_snapshot();
        let connected_ids =
            Self::connected_client_ids_lock(&self.connected_client_ids);
        clients
            .into_iter()
            .filter(|client| connected_ids.contains(&Self::client_key(client)))
            .collect()
    }

    fn begin_publication_operation(
        &self,
        dirty: PublicationDirtyKey,
    ) -> PublicationOperationGuard {
        Self::begin_publication_operation_from_state(
            &self.client_publication_state,
            dirty,
        )
    }

    fn begin_publication_operation_from_state(
        state: &Arc<Mutex<ClientPublicationState>>,
        dirty: PublicationDirtyKey,
    ) -> PublicationOperationGuard {
        let generation = {
            let mut state =
                state.lock().unwrap_or_else(|poison| poison.into_inner());
            let generation = state.next_generation();
            state.active_operations.insert(generation, dirty);
            for attach in state.attaching_clients.values_mut() {
                attach.blockers.insert(generation);
            }
            state.mark_dirty(dirty);
            generation
        };
        PublicationOperationGuard {
            state: state.clone(),
            generation: Some(generation),
        }
    }

    fn register_attaching_client(
        state: &Arc<Mutex<ClientPublicationState>>,
        client_key: usize,
        transport: PubsubTransport,
    ) -> AttachingClientGuard {
        Self::register_attaching_client_inner(
            state, client_key, transport, false, false, true,
        )
        .expect("replacement attach registration must succeed")
    }

    fn try_register_websocket_policy_refresh(
        state: &Arc<Mutex<ClientPublicationState>>,
        client_key: usize,
    ) -> Option<AttachingClientGuard> {
        Self::register_attaching_client_inner(
            state,
            client_key,
            PubsubTransport::WebSocket,
            true,
            true,
            false,
        )
    }

    fn register_attaching_client_inner(
        state: &Arc<Mutex<ClientPublicationState>>,
        client_key: usize,
        transport: PubsubTransport,
        refresh_account_policy: bool,
        serialize_account_refresh: bool,
        replace_existing: bool,
    ) -> Option<AttachingClientGuard> {
        let mut state_guard =
            state.lock().unwrap_or_else(|poison| poison.into_inner());
        if !replace_existing
            && state_guard.attaching_clients.contains_key(&client_key)
        {
            return None;
        }
        let attempt = state_guard.next_generation();
        let notify = Arc::new(Notify::new());
        let mut dirty_accounts = HashSet::new();
        let mut dirty_programs = HashSet::new();
        let mut blockers = HashSet::new();
        for (generation, dirty) in &state_guard.active_operations {
            blockers.insert(*generation);
            match dirty {
                PublicationDirtyKey::Account(pubkey) => {
                    dirty_accounts.insert(*pubkey);
                }
                PublicationDirtyKey::Program(program_id) => {
                    dirty_programs.insert(*program_id);
                }
            }
        }
        let replaced = state_guard.attaching_clients.insert(
            client_key,
            AttachingClientPublication {
                attempt,
                transport,
                dirty_accounts,
                dirty_programs,
                blockers,
                refresh_account_policy,
                serialize_account_refresh,
                notify: notify.clone(),
            },
        );
        if let Some(replaced) = replaced {
            replaced.notify.notify_one();
        }
        Some(AttachingClientGuard {
            state: state.clone(),
            client_key,
            attempt,
            notify,
            armed: true,
        })
    }

    fn mark_websocket_policy_refresh(state: &mut ClientPublicationState) {
        for attach in state.attaching_clients.values_mut() {
            if attach.transport == PubsubTransport::WebSocket {
                attach.refresh_account_policy = true;
                attach.notify.notify_one();
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn spawn_connected_websocket_policy_refresh<
        U: SubscribedAccountsTracker,
    >(
        all_clients: Arc<Mutex<Vec<Arc<T>>>>,
        accounts_tracker: Arc<U>,
        program_subs: Arc<Mutex<HashSet<Pubkey>>>,
        grpc_only_subscriptions: Arc<Mutex<HashSet<Pubkey>>>,
        unsubscribed_accounts: Arc<Mutex<HashSet<Pubkey>>>,
        subscription_operation_locks: SubscriptionOperationLocks,
        client_publication_state: Arc<Mutex<ClientPublicationState>>,
        connected_client_ids: Arc<Mutex<HashSet<usize>>>,
        never_debounce: HashSet<Pubkey>,
    ) {
        let connected_ids =
            Self::connected_client_ids_lock(&connected_client_ids).clone();
        let websocket_clients: Vec<_> = all_clients
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .iter()
            .filter(|client| {
                client.transport() == PubsubTransport::WebSocket
                    && connected_ids.contains(&Self::client_key(client))
            })
            .cloned()
            .collect();

        for client in websocket_clients {
            let Some(attach_guard) =
                Self::try_register_websocket_policy_refresh(
                    &client_publication_state,
                    Self::client_key(&client),
                )
            else {
                continue;
            };
            let accounts_tracker = accounts_tracker.clone();
            let program_subs = program_subs.clone();
            let grpc_only_subscriptions = grpc_only_subscriptions.clone();
            let unsubscribed_accounts = unsubscribed_accounts.clone();
            let subscription_operation_locks =
                subscription_operation_locks.clone();
            let client_publication_state = client_publication_state.clone();
            let all_clients = all_clients.clone();
            let connected_client_ids = connected_client_ids.clone();
            let never_debounce = never_debounce.clone();
            tokio::spawn(async move {
                if let Err(err) = Self::catch_up_and_publish_attaching_client(
                    &client,
                    &accounts_tracker,
                    &program_subs,
                    &grpc_only_subscriptions,
                    &unsubscribed_accounts,
                    &subscription_operation_locks,
                    &client_publication_state,
                    &all_clients,
                    &connected_client_ids,
                    &never_debounce,
                    attach_guard,
                )
                .await
                {
                    warn!(
                        client_id = %client.id(),
                        error = ?err,
                        "Failed to refresh websocket subscription policy after gRPC topology change"
                    );
                }
            });
        }
    }

    fn pending_provider_policies(
        state: &Arc<Mutex<ClientPublicationState>>,
    ) -> HashMap<Pubkey, AccountSubscriptionPublicationPolicy> {
        state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .provider_publications
            .iter()
            .filter_map(|(pubkey, publications)| {
                publications
                    .last()
                    .map(|publication| (*pubkey, publication.policy))
            })
            .collect()
    }

    fn settled_program_subscriptions(
        program_subs: &Arc<Mutex<HashSet<Pubkey>>>,
        state: &Arc<Mutex<ClientPublicationState>>,
    ) -> Vec<Pubkey> {
        let state = state.lock().unwrap_or_else(|poison| poison.into_inner());
        let pending: HashSet<_> = state
            .active_operations
            .values()
            .filter_map(|dirty| match dirty {
                PublicationDirtyKey::Program(program_id) => Some(*program_id),
                PublicationDirtyKey::Account(_) => None,
            })
            .collect();
        program_subs
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .iter()
            .filter(|program_id| !pending.contains(program_id))
            .copied()
            .collect()
    }

    fn reconciliation_snapshot_from_clients(
        clients: Vec<Arc<T>>,
    ) -> Option<SubscriptionReconciliationSnapshot> {
        if clients.is_empty() {
            return None;
        }

        let mut union = HashSet::new();
        let mut intersection_sets = Vec::with_capacity(clients.len());
        for client in clients {
            let Some(snapshot) = client.subscription_reconciliation_snapshot()
            else {
                continue;
            };
            union.extend(snapshot.union);
            intersection_sets.push(snapshot.intersection);
        }

        let smallest = intersection_sets.iter().min_by_key(|set| set.len())?;
        let intersection = smallest
            .iter()
            .filter(|pubkey| {
                intersection_sets
                    .iter()
                    .filter(|set| !std::ptr::eq(*set, smallest))
                    .all(|set| set.contains(pubkey))
            })
            .copied()
            .collect();

        Some(SubscriptionReconciliationSnapshot {
            union,
            intersection,
        })
    }

    fn account_subscriptions_for_client<U: SubscribedAccountsTracker>(
        tracker: &U,
        grpc_only_subscriptions: &Arc<Mutex<HashSet<Pubkey>>>,
        unsubscribed_accounts: &Arc<Mutex<HashSet<Pubkey>>>,
        client_publication_state: &Arc<Mutex<ClientPublicationState>>,
        clients: &Mutex<Vec<Arc<T>>>,
        connected_client_ids: &Arc<Mutex<HashSet<usize>>>,
        client: &T,
    ) -> HashSet<Pubkey> {
        // The tracker is authoritative for established subscriptions. The
        // policy set is also provisional reconnect authority while a
        // gRPC-first admission is still being published to the tracker.
        let pending = Self::pending_provider_policies(client_publication_state);
        let mut subscriptions = tracker.subscribed_accounts();
        let mut effective_grpc_only = grpc_only_subscriptions
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone();
        for (pubkey, policy) in &pending {
            match policy {
                AccountSubscriptionPublicationPolicy::Absent => {
                    subscriptions.remove(pubkey);
                    effective_grpc_only.remove(pubkey);
                }
                AccountSubscriptionPublicationPolicy::Full => {
                    subscriptions.insert(*pubkey);
                    effective_grpc_only.remove(pubkey);
                }
                AccountSubscriptionPublicationPolicy::GrpcPreferred => {
                    subscriptions.insert(*pubkey);
                    effective_grpc_only.insert(*pubkey);
                }
            }
        }
        match client.transport() {
            PubsubTransport::Grpc => {
                subscriptions.extend(effective_grpc_only.iter().copied());
            }
            PubsubTransport::WebSocket => {
                let connected_grpc_clients: Vec<_> = {
                    let clients = clients
                        .lock()
                        .unwrap_or_else(|poison| poison.into_inner())
                        .clone();
                    let connected_ids =
                        Self::connected_client_ids_lock(connected_client_ids);
                    clients
                        .into_iter()
                        .filter(|client| {
                            client.transport() == PubsubTransport::Grpc
                                && connected_ids
                                    .contains(&Self::client_key(client))
                        })
                        .collect()
                };
                let grpc_fully_covered_pubkeys =
                    Self::reconciliation_snapshot_from_clients(
                        connected_grpc_clients,
                    )
                    .map(|snapshot| snapshot.intersection)
                    .unwrap_or_default();
                subscriptions.retain(|pubkey| {
                    !effective_grpc_only.contains(pubkey)
                        || !grpc_fully_covered_pubkeys.contains(pubkey)
                });
            }
        }
        let unsubscribed = unsubscribed_accounts
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        subscriptions.retain(|pubkey| {
            !unsubscribed.contains(pubkey) || pending.contains_key(pubkey)
        });
        for (pubkey, policy) in pending {
            match policy {
                AccountSubscriptionPublicationPolicy::Absent => {
                    subscriptions.remove(&pubkey);
                }
                AccountSubscriptionPublicationPolicy::Full => {
                    subscriptions.insert(pubkey);
                }
                AccountSubscriptionPublicationPolicy::GrpcPreferred => {
                    if client.transport() == PubsubTransport::Grpc {
                        subscriptions.insert(pubkey);
                    }
                }
            }
        }
        subscriptions
    }

    fn remove_client(&self, target: &Arc<T>) {
        let mut publication = self
            .client_publication_state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        {
            let mut clients = self.clients_lock();
            if let Some(pos) =
                clients.iter().position(|c| Arc::ptr_eq(c, target))
            {
                clients.swap_remove(pos);
            }
        }
        let was_connected =
            Self::connected_client_ids_lock(&self.connected_client_ids)
                .remove(&Self::client_key(target));
        if was_connected && target.transport() == PubsubTransport::Grpc {
            Self::mark_websocket_policy_refresh(&mut publication);
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn resubscribe_attaching_client_baseline<
        U: SubscribedAccountsTracker,
    >(
        client: &Arc<T>,
        accounts_tracker: &Arc<U>,
        program_subs: &Arc<Mutex<HashSet<Pubkey>>>,
        grpc_only_subscriptions: &Arc<Mutex<HashSet<Pubkey>>>,
        unsubscribed_accounts: &Arc<Mutex<HashSet<Pubkey>>>,
        client_publication_state: &Arc<Mutex<ClientPublicationState>>,
        all_clients: &Arc<Mutex<Vec<Arc<T>>>>,
        connected_client_ids: &Arc<Mutex<HashSet<usize>>>,
        never_debounce: &HashSet<Pubkey>,
    ) -> RemoteAccountProviderResult<()> {
        let programs = Self::settled_program_subscriptions(
            program_subs,
            client_publication_state,
        );
        for program_id in programs {
            client.subscribe_program(program_id).await?;
        }

        let mut account_subs = Self::account_subscriptions_for_client(
            accounts_tracker.as_ref(),
            grpc_only_subscriptions,
            unsubscribed_accounts,
            client_publication_state,
            all_clients,
            connected_client_ids,
            client.as_ref(),
        );
        account_subs.extend(never_debounce.iter().copied());
        client.resub_multiple(account_subs).await
    }

    #[allow(clippy::too_many_arguments)]
    fn account_subscription_desired_for_client<U: SubscribedAccountsTracker>(
        pubkey: &Pubkey,
        client: &T,
        accounts_tracker: &U,
        all_clients: &Arc<Mutex<Vec<Arc<T>>>>,
        connected_client_ids: &Arc<Mutex<HashSet<usize>>>,
        grpc_only_subscriptions: &Arc<Mutex<HashSet<Pubkey>>>,
        unsubscribed_accounts: &Arc<Mutex<HashSet<Pubkey>>>,
        client_publication_state: &Arc<Mutex<ClientPublicationState>>,
    ) -> bool {
        let pending_policy = client_publication_state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .pending_policy(pubkey);
        let policy = if let Some(policy) = pending_policy {
            policy
        } else {
            if unsubscribed_accounts
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .contains(pubkey)
            {
                return false;
            }
            let grpc_preferred = grpc_only_subscriptions
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .contains(pubkey);
            if !grpc_preferred && !accounts_tracker.contains(pubkey) {
                return false;
            }
            if grpc_preferred {
                AccountSubscriptionPublicationPolicy::GrpcPreferred
            } else {
                AccountSubscriptionPublicationPolicy::Full
            }
        };

        match policy {
            AccountSubscriptionPublicationPolicy::Absent => false,
            AccountSubscriptionPublicationPolicy::Full => true,
            AccountSubscriptionPublicationPolicy::GrpcPreferred => {
                if client.transport() == PubsubTransport::Grpc {
                    return true;
                }
                let connected_grpc_clients: Vec<_> = {
                    let clients = all_clients
                        .lock()
                        .unwrap_or_else(|poison| poison.into_inner())
                        .clone();
                    let connected_ids =
                        Self::connected_client_ids_lock(connected_client_ids);
                    clients
                        .into_iter()
                        .filter(|candidate| {
                            candidate.transport() == PubsubTransport::Grpc
                                && connected_ids
                                    .contains(&Self::client_key(candidate))
                        })
                        .collect()
                };
                connected_grpc_clients.is_empty()
                    || connected_grpc_clients
                        .iter()
                        .any(|grpc| !grpc.is_subscribed(pubkey))
            }
        }
    }

    async fn apply_account_subscription_to_attaching_client(
        client: &Arc<T>,
        pubkey: Pubkey,
        desired: bool,
    ) -> RemoteAccountProviderResult<()> {
        if desired {
            if client.is_subscribed(&pubkey) {
                return Ok(());
            }
            return match tokio::time::timeout(
                SUBSCRIBE_TIMEOUT,
                client.subscribe(pubkey, None),
            )
            .await
            {
                Ok(result) => result,
                Err(_) => Err(
                    RemoteAccountProviderError::AccountSubscriptionsTaskFailed(
                        format!(
                            "Attach subscribe timed out after \
                             {SUBSCRIBE_TIMEOUT:?} for client {}",
                            client.id()
                        ),
                    ),
                ),
            };
        }

        if !client.is_subscribed(&pubkey) {
            return Ok(());
        }
        match tokio::time::timeout(
            UNSUBSCRIBE_TIMEOUT,
            client.unsubscribe(pubkey),
        )
        .await
        {
            Ok(Ok(()))
            | Ok(Err(
                RemoteAccountProviderError::AccountSubscriptionDoesNotExist(_),
            )) => Ok(()),
            Ok(Err(err)) => Err(err),
            Err(_) => {
                Err(RemoteAccountProviderError::AccountSubscriptionsTaskFailed(
                    format!(
                        "Attach unsubscribe timed out after \
                         {UNSUBSCRIBE_TIMEOUT:?} for client {}",
                        client.id()
                    ),
                ))
            }
        }
    }

    fn next_attaching_client_work(
        attach_guard: &AttachingClientGuard,
        connected_client_ids: &Arc<Mutex<HashSet<usize>>>,
    ) -> AttachingClientWork {
        let mut publication = attach_guard
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let Some(attach) = publication
            .attaching_clients
            .get_mut(&attach_guard.client_key)
        else {
            return AttachingClientWork::Stale;
        };
        if attach.attempt != attach_guard.attempt {
            return AttachingClientWork::Stale;
        }
        if attach.refresh_account_policy {
            attach.refresh_account_policy = false;
            return AttachingClientWork::RefreshAccountPolicy {
                serialize: attach.serialize_account_refresh,
            };
        }
        if !attach.dirty_programs.is_empty() {
            return AttachingClientWork::Programs(
                attach.dirty_programs.drain().collect(),
            );
        }
        if !attach.dirty_accounts.is_empty() {
            return AttachingClientWork::Accounts(
                attach.dirty_accounts.drain().collect(),
            );
        }
        if !attach.blockers.is_empty() {
            return AttachingClientWork::Wait;
        }

        let transport = attach.transport;
        publication
            .attaching_clients
            .remove(&attach_guard.client_key);
        let was_disconnected =
            Self::connected_client_ids_lock(connected_client_ids)
                .insert(attach_guard.client_key);
        if was_disconnected && transport == PubsubTransport::Grpc {
            Self::mark_websocket_policy_refresh(&mut publication);
        }
        AttachingClientWork::Published { was_disconnected }
    }

    #[allow(clippy::too_many_arguments)]
    async fn catch_up_and_publish_attaching_client<
        U: SubscribedAccountsTracker,
    >(
        client: &Arc<T>,
        accounts_tracker: &Arc<U>,
        program_subs: &Arc<Mutex<HashSet<Pubkey>>>,
        grpc_only_subscriptions: &Arc<Mutex<HashSet<Pubkey>>>,
        unsubscribed_accounts: &Arc<Mutex<HashSet<Pubkey>>>,
        subscription_operation_locks: &SubscriptionOperationLocks,
        client_publication_state: &Arc<Mutex<ClientPublicationState>>,
        all_clients: &Arc<Mutex<Vec<Arc<T>>>>,
        connected_client_ids: &Arc<Mutex<HashSet<usize>>>,
        never_debounce: &HashSet<Pubkey>,
        mut attach_guard: AttachingClientGuard,
    ) -> RemoteAccountProviderResult<bool> {
        loop {
            // Register the waiter before inspecting the journal so a
            // concurrent finisher cannot notify between the check and await.
            let notified = attach_guard.notify.clone().notified_owned();
            match Self::next_attaching_client_work(
                &attach_guard,
                connected_client_ids,
            ) {
                AttachingClientWork::RefreshAccountPolicy { serialize } => {
                    let mut account_subs =
                        Self::account_subscriptions_for_client(
                            accounts_tracker.as_ref(),
                            grpc_only_subscriptions,
                            unsubscribed_accounts,
                            client_publication_state,
                            all_clients,
                            connected_client_ids,
                            client.as_ref(),
                        );
                    account_subs.extend(never_debounce.iter().copied());
                    if serialize {
                        let mut candidates = client.subscriptions_union();
                        candidates.extend(account_subs);
                        for pubkey in candidates {
                            let _operation_guard =
                                Self::subscription_operation_guard_from_map(
                                    subscription_operation_locks,
                                    pubkey,
                                )
                                .await;
                            let desired = never_debounce.contains(&pubkey)
                                || Self::account_subscription_desired_for_client(
                                    &pubkey,
                                    client.as_ref(),
                                    accounts_tracker.as_ref(),
                                    all_clients,
                                    connected_client_ids,
                                    grpc_only_subscriptions,
                                    unsubscribed_accounts,
                                    client_publication_state,
                                );
                            Self::apply_account_subscription_to_attaching_client(
                                client, pubkey, desired,
                            )
                            .await?;
                        }
                    } else {
                        client.resub_multiple(account_subs.clone()).await?;
                        let stale_accounts: Vec<_> = client
                            .subscriptions_union()
                            .difference(&account_subs)
                            .copied()
                            .collect();
                        for pubkey in stale_accounts {
                            let _operation_guard =
                                Self::subscription_operation_guard_from_map(
                                    subscription_operation_locks,
                                    pubkey,
                                )
                                .await;
                            Self::apply_account_subscription_to_attaching_client(
                                client, pubkey, false,
                            )
                            .await?;
                        }
                    }
                }
                AttachingClientWork::Programs(program_ids) => {
                    for program_id in program_ids {
                        let pending = {
                            let publication = client_publication_state
                                .lock()
                                .unwrap_or_else(|poison| poison.into_inner());
                            publication.active_operations.values().any(
                                |dirty| {
                                    matches!(
                                        dirty,
                                        PublicationDirtyKey::Program(id)
                                            if *id == program_id
                                    )
                                },
                            )
                        };
                        let desired = !pending
                            && program_subs
                                .lock()
                                .unwrap_or_else(|poison| poison.into_inner())
                                .contains(&program_id);
                        if desired {
                            client.subscribe_program(program_id).await?;
                        }
                    }
                }
                AttachingClientWork::Accounts(pubkeys) => {
                    for pubkey in pubkeys {
                        let _operation_guard =
                            Self::subscription_operation_guard_from_map(
                                subscription_operation_locks,
                                pubkey,
                            )
                            .await;
                        let desired =
                            Self::account_subscription_desired_for_client(
                                &pubkey,
                                client.as_ref(),
                                accounts_tracker.as_ref(),
                                all_clients,
                                connected_client_ids,
                                grpc_only_subscriptions,
                                unsubscribed_accounts,
                                client_publication_state,
                            );
                        Self::apply_account_subscription_to_attaching_client(
                            client, pubkey, desired,
                        )
                        .await?;
                    }
                }
                AttachingClientWork::Wait => notified.await,
                AttachingClientWork::Published { was_disconnected } => {
                    attach_guard.disarm();
                    if was_disconnected
                        && client.transport() == PubsubTransport::Grpc
                    {
                        Self::spawn_connected_websocket_policy_refresh(
                            all_clients.clone(),
                            accounts_tracker.clone(),
                            program_subs.clone(),
                            grpc_only_subscriptions.clone(),
                            unsubscribed_accounts.clone(),
                            subscription_operation_locks.clone(),
                            client_publication_state.clone(),
                            connected_client_ids.clone(),
                            never_debounce.clone(),
                        );
                    }
                    return Ok(was_disconnected);
                }
                AttachingClientWork::Stale => {
                    return Err(
                        RemoteAccountProviderError::AccountSubscriptionsTaskFailed(
                            format!(
                                "Attach publication attempt for client {} \
                                 was superseded",
                                client.id()
                            ),
                        ),
                    );
                }
            }
        }
    }

    pub(crate) async fn add_client<U: SubscribedAccountsTracker>(
        &self,
        client: Arc<T>,
        abort_rx: mpsc::Receiver<()>,
        subscribed_accounts_tracker: Arc<U>,
    ) -> RemoteAccountProviderResult<()> {
        let attach_guard = Self::register_attaching_client(
            &self.client_publication_state,
            Self::client_key(&client),
            client.transport(),
        );
        {
            let mut clients = self.clients_lock();
            clients.push(client.clone());
        }

        if let Err(err) = Self::resubscribe_attaching_client_baseline(
            &client,
            &subscribed_accounts_tracker,
            &self.program_subs,
            &self.grpc_only_subscriptions,
            &self.unsubscribed_accounts,
            &self.client_publication_state,
            &self.clients,
            &self.connected_client_ids,
            &self.never_debounce,
        )
        .await
        {
            self.remove_client(&client);
            return Err(err);
        }

        let was_disconnected =
            match Self::catch_up_and_publish_attaching_client(
                &client,
                &subscribed_accounts_tracker,
                &self.program_subs,
                &self.grpc_only_subscriptions,
                &self.unsubscribed_accounts,
                &self.subscription_operation_locks,
                &self.client_publication_state,
                &self.clients,
                &self.connected_client_ids,
                &self.never_debounce,
                attach_guard,
            )
            .await
            {
                Ok(was_disconnected) => was_disconnected,
                Err(err) => {
                    self.remove_client(&client);
                    return Err(err);
                }
            };

        if self.forwarders_started.load(Ordering::SeqCst) {
            self.spawn_forwarder_for_client(
                &client,
                self.dedup_window,
                self.debounce_interval,
                self.debounce_detection_window,
                self.allowed_in_debounce_window_count(),
            );
        }

        if was_disconnected {
            let connected = self
                .connected_clients
                .fetch_add(1, Ordering::SeqCst)
                .saturating_add(1);
            metrics::set_connected_pubsub_clients_count(connected as usize);
            if client.subs_immediately() {
                let connected = self
                    .connected_clients_subscribing_immediately
                    .fetch_add(1, Ordering::SeqCst)
                    .saturating_add(1);
                metrics::set_connected_direct_pubsub_clients_count(
                    connected as usize,
                );
            }
        }
        metrics::set_pubsub_client_uptime(client.id(), true);
        if let Some(delay_ms) = client.current_resub_delay_ms() {
            metrics::set_pubsub_client_resubscribe_delay(client.id(), delay_ms);
        }

        Self::spawn_reconnectors(
            vec![(client.clone(), abort_rx)],
            self.clients.clone(),
            subscribed_accounts_tracker.clone(),
            self.program_subs.clone(),
            self.grpc_only_subscriptions.clone(),
            self.unsubscribed_accounts.clone(),
            self.subscription_operation_locks.clone(),
            self.client_publication_state.clone(),
            self.never_debounce.clone(),
            self.connected_client_ids.clone(),
            self.connected_clients.clone(),
            self.connected_clients_subscribing_immediately.clone(),
        );
        Ok(())
    }

    fn clients_lock(&self) -> MutexGuard<'_, Vec<Arc<T>>> {
        // Lock poisoning means a thread panicked while mutating mux state;
        // treating that as unrecoverable is safer than continuing with it.
        self.clients.lock().expect("clients lock poisoned")
    }

    async fn subscription_operation_guard(
        &self,
        pubkey: Pubkey,
    ) -> SubscriptionKeyGuard {
        Self::subscription_operation_guard_from_map(
            &self.subscription_operation_locks,
            pubkey,
        )
        .await
    }

    async fn subscription_operation_guard_from_map(
        subscription_operation_locks: &SubscriptionOperationLocks,
        pubkey: Pubkey,
    ) -> SubscriptionKeyGuard {
        subscription_operation_locks.acquire(pubkey).await
    }

    fn program_subs_lock(&self) -> MutexGuard<'_, HashSet<Pubkey>> {
        self.program_subs
            .lock()
            .expect("program_subs lock poisoned")
    }

    #[instrument(
        skip(
            client,
            all_clients,
            accounts_tracker,
            program_subs,
            grpc_only_subscriptions,
            unsubscribed_accounts,
            subscription_operation_locks,
            client_publication_state,
            never_debounce,
            connected_client_ids,
            connected_clients,
            connected_clients_subscribing_immediately
        ),
        fields(client_id = %client.id())
    )]
    #[allow(clippy::too_many_arguments)]
    async fn reconnect_client_with_backoff<U: SubscribedAccountsTracker>(
        client: Arc<T>,
        all_clients: Arc<Mutex<Vec<Arc<T>>>>,
        accounts_tracker: Arc<U>,
        program_subs: Arc<Mutex<HashSet<Pubkey>>>,
        grpc_only_subscriptions: Arc<Mutex<HashSet<Pubkey>>>,
        unsubscribed_accounts: Arc<Mutex<HashSet<Pubkey>>>,
        subscription_operation_locks: SubscriptionOperationLocks,
        client_publication_state: Arc<Mutex<ClientPublicationState>>,
        never_debounce: HashSet<Pubkey>,
        connected_client_ids: Arc<Mutex<HashSet<usize>>>,
        connected_clients: Arc<AtomicU16>,
        connected_clients_subscribing_immediately: Arc<AtomicU16>,
    ) {
        fn fib_with_max_secs(n: u64) -> u64 {
            let (mut a, mut b) = (0u64, 1u64);
            for _ in 0..n {
                (a, b) = (b, a.saturating_add(b));
            }
            // 1h max wait
            a.min(3_600)
        }

        const WARN_EVERY_ATTEMPTS: u64 = 10;
        let mut attempt = 0;
        loop {
            attempt += 1;
            // Track the current resubscription delay for this client
            if let Some(delay_ms) = client.current_resub_delay_ms() {
                metrics::set_pubsub_client_resubscribe_delay(
                    client.id(),
                    delay_ms,
                );
            }
            match Self::reconnect_client(
                client.clone(),
                &all_clients,
                &accounts_tracker,
                &program_subs,
                &grpc_only_subscriptions,
                &unsubscribed_accounts,
                &subscription_operation_locks,
                &client_publication_state,
                &never_debounce,
                connected_client_ids.clone(),
                connected_clients.clone(),
                connected_clients_subscribing_immediately.clone(),
            )
            .await
            {
                Ok(()) => {
                    // Reset metrics on successful reconnect
                    metrics::set_pubsub_client_reconnect_backoff_duration_seconds(
                        client.id(),
                        0,
                    );
                    metrics::set_pubsub_client_failed_reconnect_attempts(
                        client.id(),
                        0,
                    );
                    debug!(
                        client_id = %client.id(),
                        attempt,
                        "Successfully reconnected client"
                    );
                    break;
                }
                Err(err) => {
                    let wait_duration =
                        Duration::from_secs(fib_with_max_secs(attempt));
                    // Update backoff duration metric
                    metrics::set_pubsub_client_reconnect_backoff_duration_seconds(
                        client.id(),
                        wait_duration.as_secs(),
                    );
                    // Record current failed attempt count after the failed attempt
                    metrics::set_pubsub_client_failed_reconnect_attempts(
                        client.id(),
                        attempt,
                    );
                    // Log at max once per minute or every WARN_EVERY_ATTEMPTS attempts
                    if attempt % WARN_EVERY_ATTEMPTS == 0
                        || wait_duration.as_secs() >= 60
                    {
                        warn!(
                            client_id = %client.id(),
                            attempt,
                            wait_duration = ?wait_duration,
                            error = ?err,
                            "Failed to reconnect client, will retry after backoff"
                        );
                    }
                    tokio::time::sleep(wait_duration).await;
                    debug!(
                        client_id = %client.id(),
                        attempt,
                        wait_duration = ?wait_duration,
                        error = ?err,
                        "Reconnect attempt failed, will retry"
                    );
                }
            }
        }
    }

    #[instrument(
        skip(client, all_clients, accounts_tracker, program_subs, grpc_only_subscriptions, unsubscribed_accounts, subscription_operation_locks, client_publication_state, never_debounce, connected_client_ids, connected_clients, connected_clients_subscribing_immediately),
        fields(client_id = %client.id())
    )]
    #[allow(clippy::too_many_arguments)]
    async fn reconnect_client<U: SubscribedAccountsTracker>(
        client: Arc<T>,
        all_clients: &Arc<Mutex<Vec<Arc<T>>>>,
        accounts_tracker: &Arc<U>,
        program_subs: &Arc<Mutex<HashSet<Pubkey>>>,
        grpc_only_subscriptions: &Arc<Mutex<HashSet<Pubkey>>>,
        unsubscribed_accounts: &Arc<Mutex<HashSet<Pubkey>>>,
        subscription_operation_locks: &SubscriptionOperationLocks,
        client_publication_state: &Arc<Mutex<ClientPublicationState>>,
        never_debounce: &HashSet<Pubkey>,
        connected_client_ids: Arc<Mutex<HashSet<usize>>>,
        connected_clients: Arc<AtomicU16>,
        connected_clients_subscribing_immediately: Arc<AtomicU16>,
    ) -> RemoteAccountProviderResult<()> {
        let attach_guard = Self::register_attaching_client(
            client_publication_state,
            Self::client_key(&client),
            client.transport(),
        );
        if let Err(err) = client.try_reconnect().await {
            debug!(
                client_id = %client.id(),
                error = ?err,
                "Failed to reconnect client"
            );
            return Err(err);
        }

        if let Err(err) = Self::resubscribe_attaching_client_baseline(
            &client,
            accounts_tracker,
            program_subs,
            grpc_only_subscriptions,
            unsubscribed_accounts,
            client_publication_state,
            all_clients,
            &connected_client_ids,
            never_debounce,
        )
        .await
        {
            debug!(
                client_id = %client.id(),
                resub_delay_ms = ?client.current_resub_delay_ms(),
                error = ?err,
                "Failed to resubscribe accounts after reconnect"
            );
            return Err(err);
        }

        let was_disconnected = Self::catch_up_and_publish_attaching_client(
            &client,
            accounts_tracker,
            program_subs,
            grpc_only_subscriptions,
            unsubscribed_accounts,
            subscription_operation_locks,
            client_publication_state,
            all_clients,
            &connected_client_ids,
            never_debounce,
            attach_guard,
        )
        .await?;
        if was_disconnected {
            connected_clients.fetch_add(1, Ordering::SeqCst);
            metrics::set_connected_pubsub_clients_count(
                connected_clients.load(Ordering::SeqCst) as usize,
            );
        }
        metrics::set_pubsub_client_uptime(client.id(), true);
        if was_disconnected && client.subs_immediately() {
            let previous = connected_clients_subscribing_immediately
                .fetch_add(1, Ordering::SeqCst);
            let current = previous.saturating_add(1);
            metrics::set_connected_direct_pubsub_clients_count(
                current as usize,
            );
        }

        Ok(())
    }

    fn spawn_dedup_pruner(&self) {
        let window = self.dedup_window;
        let cache = self.dedup_cache.clone();
        let shutdown = self.shutdown_token.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = tokio::time::sleep(window) => {
                        let now = Instant::now();
                        let mut map = cache.lock().unwrap();
                        map.retain(|_, ts| now.duration_since(*ts) <= window);
                    }
                    _ = shutdown.cancelled() => break,
                }
            }
        });
    }

    fn spawn_debounce_flusher(&self) {
        // This task periodically scans all debounce states and
        // forwards any pending update whose next_allowed_forward has arrived.
        // It runs roughly every debounce_interval/4 (with a minimum of 10ms).
        //
        // It is not 100% exact: a pending update may be forwarded up to ~debounce_interval/4 later
        // than the exact moment it becomes eligible.
        // This inaccuracy only matters when we receive a burst of updates for an account and then
        // no more for up to a fourth the interval.
        //
        // The trade-off significantly reduces task churn and memory usage compared to per-update
        // timers, while preserving the core contract: we coalesce high-frequency streams to at
        // most one update per debounce interval, always forwarding the latest pending state.
        let states = self.debounce_states.clone();
        let out_tx = self.out_tx.clone();
        let interval = self.debounce_interval;
        let shutdown = self.shutdown_token.clone();
        tokio::spawn(async move {
            let tick = cmp::max(Duration::from_millis(10), interval / 4);
            loop {
                tokio::select! {
                    _ = tokio::time::sleep(tick) => {
                        let now = Instant::now();
                        let mut to_forward = vec![];
                        {
                            let mut map =
                                states.lock().expect("debounce_states lock poisoned");
                            for debounce_state in map.values_mut() {
                                if let DebounceState::Enabled {
                                    next_allowed_forward,
                                    pending,
                                    ..
                                } = debounce_state
                                {
                                    if now >= *next_allowed_forward {
                                        if let Some(u) = pending.take() {
                                            *next_allowed_forward = now + interval;
                                            to_forward.push(u);
                                        }
                                    }
                                }
                            }
                        }
                        for update in to_forward {
                            let _ = out_tx.send(update).await;
                        }
                    }
                    _ = shutdown.cancelled() => break,
                }
            }
        });
    }

    fn start_forwarders(&self) {
        let window = self.dedup_window;
        let debounce_interval = self.debounce_interval;
        let detection_window = self.debounce_detection_window;
        let allowed_count = self.allowed_in_debounce_window_count();

        self.forwarders_started.store(true, Ordering::SeqCst);
        for client in self.clients_snapshot() {
            self.spawn_forwarder_for_client(
                &client,
                window,
                debounce_interval,
                detection_window,
                allowed_count,
            );
        }
    }

    fn spawn_forwarder_for_client(
        &self,
        client: &Arc<T>,
        window: Duration,
        debounce_interval: Duration,
        detection_window: Duration,
        allowed_count: usize,
    ) {
        let mut inner_rx = client.take_updates();
        let params = ForwarderParams {
            tx: self.out_tx.clone(),
            cache: self.dedup_cache.clone(),
            debounce_states: self.debounce_states.clone(),
            window,
            debounce_interval,
            detection_window,
            allowed_count,
        };
        let never_debounce = self.never_debounce.clone();
        tokio::spawn(async move {
            Self::forwarder_loop(&mut inner_rx, params, never_debounce).await;
        });
    }

    async fn forwarder_loop(
        inner_rx: &mut mpsc::Receiver<SubscriptionUpdate>,
        params: ForwarderParams,
        never_debounce: HashSet<Pubkey>,
    ) {
        while let Some(update) = inner_rx.recv().await {
            let now = Instant::now();
            let key = (update.pubkey, update.slot);
            if !Self::should_forward_dedup(
                &params.cache,
                key,
                now,
                params.window,
            ) {
                continue;
            }
            if never_debounce.contains(&update.pubkey) {
                let _ = params.tx.send(update).await;
            } else if let Some(u) = Self::handle_debounce_and_maybe_forward(
                &params.debounce_states,
                update,
                now,
                params.detection_window,
                params.debounce_interval,
                params.allowed_count,
            ) {
                let _ = params.tx.send(u).await;
            }
        }
    }

    fn should_forward_dedup(
        cache: &Arc<Mutex<HashMap<(Pubkey, u64), Instant>>>,
        key: (Pubkey, u64),
        now: Instant,
        window: Duration,
    ) -> bool {
        let mut map = cache.lock().unwrap();
        match map.get_mut(&key) {
            Some(ts) => {
                if now.duration_since(*ts) > window {
                    *ts = now;
                    true
                } else {
                    false
                }
            }
            None => {
                map.insert(key, now);
                true
            }
        }
    }

    fn handle_debounce_and_maybe_forward(
        debounce_states: &Arc<Mutex<HashMap<Pubkey, DebounceState>>>,
        update: SubscriptionUpdate,
        now: Instant,
        detection_window: Duration,
        debounce_interval: Duration,
        allowed_count: usize,
    ) -> Option<SubscriptionUpdate> {
        let pubkey = update.pubkey;
        let mut maybe_forward_now = None;
        {
            let mut states = debounce_states
                .lock()
                .expect("debounce_states lock poisoned");
            let debounce_state = states.entry(pubkey).or_insert_with(|| {
                DebounceState::Disabled {
                    pubkey,
                    arrivals: VecDeque::new(),
                }
            });

            // prune and push current
            let arrivals_len = {
                let arrivals = debounce_state.arrivals_mut();
                while let Some(&front) = arrivals.front() {
                    if now.duration_since(front) > detection_window {
                        arrivals.pop_front();
                    } else {
                        break;
                    }
                }
                arrivals.push_back(now);
                arrivals.len()
            };

            let enable = if arrivals_len >= allowed_count {
                let arrivals = debounce_state.arrivals_ref();
                let spans_ok = {
                    let len = arrivals.len();
                    if len < allowed_count {
                        false
                    } else {
                        let start = len - allowed_count;
                        let window_slice: Vec<Instant> =
                            arrivals.iter().skip(start).cloned().collect();
                        window_slice.windows(2).all(|w| {
                            let dt = w[1].saturating_duration_since(w[0]);
                            dt <= debounce_interval
                        })
                    }
                };
                spans_ok
            } else {
                false
            };

            if arrivals_len > allowed_count {
                let arrivals = debounce_state.arrivals_mut();
                while arrivals.len() > allowed_count {
                    arrivals.pop_front();
                }
            }

            let changed = if enable {
                debounce_state.maybe_enable(now)
            } else {
                debounce_state.maybe_disable()
            };
            if changed && tracing::enabled!(tracing::Level::TRACE) {
                trace!(
                    pubkey = %pubkey,
                    state = %debounce_state.label(),
                    "Debounce state"
                );
            }

            match debounce_state {
                DebounceState::Disabled { .. } => {
                    maybe_forward_now = Some(update);
                }
                DebounceState::Enabled {
                    next_allowed_forward,
                    pending,
                    ..
                } => {
                    if now >= *next_allowed_forward {
                        *next_allowed_forward = now + debounce_interval;
                        *pending = None;
                        maybe_forward_now = Some(update);
                    } else {
                        *pending = Some(update);
                    }
                }
            }
        }
        maybe_forward_now
    }

    /// Number of clients that must confirm an account subscription for it to be considered active.
    /// 2/3 of connected clients subscribing immediately.
    fn required_account_subscription_confirmations(&self) -> usize {
        let n = self
            .connected_clients_subscribing_immediately
            .load(Ordering::SeqCst) as usize;
        cmp::max(1, (n * 2) / 3)
    }

    /// Number of clients that must confirm a program subscription for it to be considered
    /// active.
    /// 1/3 of connected clients subscribing immediately.
    fn required_program_subscription_confirmations(&self) -> usize {
        let n = self
            .connected_clients_subscribing_immediately
            .load(Ordering::SeqCst) as usize;
        cmp::max(1, n / 3)
    }

    fn allowed_in_debounce_window_count(&self) -> usize {
        (self.debounce_detection_window.as_millis()
            / self.debounce_interval.as_millis()) as usize
    }

    #[cfg(test)]
    fn get_debounce_state(&self, pubkey: Pubkey) -> Option<DebounceState> {
        let states = self
            .debounce_states
            .lock()
            .expect("debounce_states lock poisoned");
        states.get(&pubkey).cloned()
    }

    #[cfg(test)]
    pub(crate) fn grpc_preference_completions(&self) -> u64 {
        self.grpc_preference_completions.load(Ordering::SeqCst)
    }
}

impl<T> Clone for SubMuxClient<T>
where
    T: ChainPubsubClient + ReconnectableClient,
{
    fn clone(&self) -> Self {
        Self {
            clients: self.clients.clone(),
            out_tx: self.out_tx.clone(),
            out_rx: self.out_rx.clone(),
            dedup_cache: self.dedup_cache.clone(),
            dedup_window: self.dedup_window,
            debounce_interval: self.debounce_interval,
            debounce_detection_window: self.debounce_detection_window,
            debounce_states: self.debounce_states.clone(),
            never_debounce: self.never_debounce.clone(),
            program_subs: self.program_subs.clone(),
            unconfirmed_program_subs: self.unconfirmed_program_subs.clone(),
            grpc_only_subscriptions: self.grpc_only_subscriptions.clone(),
            unsubscribed_accounts: self.unsubscribed_accounts.clone(),
            subscription_operation_locks: self
                .subscription_operation_locks
                .clone(),
            client_publication_state: self.client_publication_state.clone(),
            #[cfg(test)]
            grpc_preference_completions: self
                .grpc_preference_completions
                .clone(),
            connected_client_ids: self.connected_client_ids.clone(),
            connected_clients: self.connected_clients.clone(),
            connected_clients_subscribing_immediately: self
                .connected_clients_subscribing_immediately
                .clone(),
            forwarders_started: self.forwarders_started.clone(),
            shutdown_token: self.shutdown_token.clone(),
            cancel_on_drop: false,
        }
    }
}

impl<T> Drop for SubMuxClient<T>
where
    T: ChainPubsubClient + ReconnectableClient,
{
    fn drop(&mut self) {
        if self.cancel_on_drop {
            self.shutdown_token.cancel();
        }
    }
}

#[async_trait]
impl<T> ChainPubsubClient for SubMuxClient<T>
where
    T: ChainPubsubClient + ReconnectableClient,
{
    async fn subscribe(
        &self,
        pubkey: Pubkey,
        retries: Option<usize>,
    ) -> RemoteAccountProviderResult<()> {
        let publication_operation = self
            .begin_publication_operation(PublicationDirtyKey::Account(pubkey));
        let settlement_guard = self.subscription_operation_guard(pubkey).await;
        let was_unsubscribed = self
            .unsubscribed_accounts
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .remove(&pubkey);
        let tombstone_guard = OwnedSubscriptionSetGuard {
            subscriptions: self.unsubscribed_accounts.clone(),
            pubkey,
            rollback_on_drop: was_unsubscribed
                .then_some(SubscriptionSetRollback::Insert),
        };
        let was_grpc_only = self
            .grpc_only_subscriptions
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .remove(&pubkey);
        let policy_guard = OwnedSubscriptionSetGuard {
            subscriptions: self.grpc_only_subscriptions.clone(),
            pubkey,
            rollback_on_drop: was_grpc_only
                .then_some(SubscriptionSetRollback::Insert),
        };
        AccountSubscriptionTask::Subscribe(
            pubkey,
            retries,
            self.required_account_subscription_confirmations(),
        )
        .process_with_settlement_guard(
            self.connected_clients_snapshot(),
            settlement_guard,
            move |settlement| {
                let mut policy_guard = policy_guard;
                let mut tombstone_guard = tombstone_guard;
                if settlement.quorum_succeeded {
                    policy_guard.commit();
                    tombstone_guard.commit();
                }
                drop(publication_operation);
            },
        )
        .await
    }

    async fn subscribe_program(
        &self,
        program_id: Pubkey,
    ) -> RemoteAccountProviderResult<()> {
        let publication_operation = self.begin_publication_operation(
            PublicationDirtyKey::Program(program_id),
        );
        let settlement_guard =
            self.subscription_operation_guard(program_id).await;
        let was_existing = self.program_subs_lock().contains(&program_id);
        let needs_retry = self
            .unconfirmed_program_subs
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .contains(&program_id);
        if was_existing && !needs_retry {
            debug!(program_id = %program_id, "Program subscription already exists");
            return Ok(());
        }

        self.program_subs_lock().insert(program_id);
        self.unconfirmed_program_subs
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .insert(program_id);
        let clients = self.connected_clients_snapshot();
        let required_confirmations =
            self.required_program_subscription_confirmations();
        let program_subs = self.program_subs.clone();
        let unconfirmed_program_subs = self.unconfirmed_program_subs.clone();
        AccountSubscriptionTask::SubscribeProgram(
            program_id,
            required_confirmations,
        )
        .process_with_settlement_guard(
            clients,
            settlement_guard,
            move |settlement| {
                if settlement.quorum_succeeded {
                    unconfirmed_program_subs
                        .lock()
                        .unwrap_or_else(|poison| poison.into_inner())
                        .remove(&program_id);
                } else if settlement.successful_legs == 0 && !was_existing {
                    program_subs
                        .lock()
                        .unwrap_or_else(|poison| poison.into_inner())
                        .remove(&program_id);
                    unconfirmed_program_subs
                        .lock()
                        .unwrap_or_else(|poison| poison.into_inner())
                        .remove(&program_id);
                }
                drop(publication_operation);
            },
        )
        .await
    }

    async fn unsubscribe(
        &self,
        pubkey: Pubkey,
    ) -> RemoteAccountProviderResult<()> {
        let publication_operation = self
            .begin_publication_operation(PublicationDirtyKey::Account(pubkey));
        let settlement_guard = self.subscription_operation_guard(pubkey).await;
        let inserted_tombstone = self
            .unsubscribed_accounts
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .insert(pubkey);
        let tombstone_guard = OwnedSubscriptionSetGuard {
            subscriptions: self.unsubscribed_accounts.clone(),
            pubkey,
            rollback_on_drop: inserted_tombstone
                .then_some(SubscriptionSetRollback::Remove),
        };
        let was_grpc_only = self
            .grpc_only_subscriptions
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .remove(&pubkey);
        let policy_guard = OwnedSubscriptionSetGuard {
            subscriptions: self.grpc_only_subscriptions.clone(),
            pubkey,
            rollback_on_drop: was_grpc_only
                .then_some(SubscriptionSetRollback::Insert),
        };
        let clients = self.connected_clients_snapshot();
        AccountSubscriptionTask::Unsubscribe(pubkey)
            .process_with_settlement_guard(
                clients,
                settlement_guard,
                move |settlement| {
                    let mut tombstone_guard = tombstone_guard;
                    let mut policy_guard = policy_guard;
                    if settlement.quorum_succeeded {
                        tombstone_guard.commit();
                        policy_guard.commit();
                    }
                    drop(publication_operation);
                },
            )
            .await
    }

    async fn prefer_grpc_subscription(
        &self,
        pubkey: Pubkey,
    ) -> RemoteAccountProviderResult<()> {
        let _publication_operation = self
            .begin_publication_operation(PublicationDirtyKey::Account(pubkey));
        let _settlement_guard = self.subscription_operation_guard(pubkey).await;
        let was_unsubscribed = self
            .unsubscribed_accounts
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .remove(&pubkey);
        let mut tombstone_guard = SubscriptionSetGuard {
            subscriptions: &self.unsubscribed_accounts,
            pubkey,
            rollback_on_drop: was_unsubscribed
                .then_some(SubscriptionSetRollback::Insert),
        };
        // Publish the desired policy before inspecting clients. Attaching and
        // reconnecting gRPC clients include this provisional key even before
        // the provider publishes it to its authoritative LRU tracker.
        let inserted_policy = self
            .grpc_only_subscriptions
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .insert(pubkey);
        let mut provisional_policy = SubscriptionSetGuard {
            subscriptions: &self.grpc_only_subscriptions,
            pubkey,
            rollback_on_drop: inserted_policy
                .then_some(SubscriptionSetRollback::Remove),
        };
        let mut clients = self.connected_clients_snapshot();
        let websocket_covered = || {
            clients.iter().any(|client| {
                client.transport() == PubsubTransport::WebSocket
                    && client.is_subscribed(&pubkey)
            })
        };
        let has_grpc = clients
            .iter()
            .any(|client| client.transport() == PubsubTransport::Grpc);
        let grpc_fully_covered = has_grpc
            && clients
                .iter()
                .filter(|client| client.transport() == PubsubTransport::Grpc)
                .all(|client| client.is_subscribed(&pubkey));

        // The desired policy is already in place. This makes repeated
        // not-found classification and reconciliation calls free of
        // transport work.
        if grpc_fully_covered && !websocket_covered() {
            provisional_policy.commit();
            tombstone_guard.commit();
            #[cfg(test)]
            self.grpc_preference_completions
                .fetch_add(1, Ordering::SeqCst);
            return Ok(());
        }

        // Only drop websocket legs once every connected gRPC client holds
        // the subscription; otherwise leave existing coverage untouched.
        // Calls run concurrently and bounded so a stalled client cannot
        // wedge subscription transitions.
        let grpc_results = futures_util::future::join_all(
            clients
                .iter()
                .filter(|client| {
                    client.transport() == PubsubTransport::Grpc
                })
                .filter(|client| !client.is_subscribed(&pubkey))
                .map(|client| async move {
                        match tokio::time::timeout(
                            SUBSCRIBE_TIMEOUT,
                            client.subscribe(pubkey, None),
                        )
                        .await
                        {
                            Ok(result) => result,
                            Err(_) => Err(
                                RemoteAccountProviderError::AccountSubscriptionsTaskFailed(
                                    format!(
                                        "Subscribe timed out after {SUBSCRIBE_TIMEOUT:?} for client {}",
                                        client.id()
                                    ),
                                ),
                            ),
                        }
                    }),
        )
        .await;
        let mut last_err = None;
        for result in grpc_results {
            if let Err(err) = result {
                last_err = Some(err);
            }
        }
        // A no-op subscribe can mask lost coverage; trust only the
        // post-subscribe client state.
        clients = self.connected_clients_snapshot();
        let has_grpc = clients
            .iter()
            .any(|client| client.transport() == PubsubTransport::Grpc);
        let grpc_fully_covered = has_grpc
            && clients
                .iter()
                .filter(|client| client.transport() == PubsubTransport::Grpc)
                .all(|client| client.is_subscribed(&pubkey));
        if !grpc_fully_covered {
            return Err(last_err.unwrap_or_else(|| {
                RemoteAccountProviderError::AccountSubscriptionsTaskFailed(
                    format!(
                        "Not every connected gRPC client holds subscription for {pubkey}"
                    ),
                )
            }));
        }

        futures_util::future::join_all(
            clients
                .iter()
                .filter(|c| c.transport() == PubsubTransport::WebSocket)
                .filter(|client| client.is_subscribed(&pubkey))
                .map(|client| async move {
                    match tokio::time::timeout(
                        UNSUBSCRIBE_TIMEOUT,
                        client.unsubscribe(pubkey),
                    )
                    .await
                    {
                        Ok(Ok(()))
                        | Ok(Err(
                            RemoteAccountProviderError::AccountSubscriptionDoesNotExist(
                                _,
                            ),
                        )) => {}
                        Ok(Err(err)) => {
                            warn!(
                                pubkey = %pubkey,
                                client_id = %client.id(),
                                error = ?err,
                                "Failed to drop websocket leg for gRPC-only coverage"
                            );
                        }
                        Err(_) => {
                            warn!(
                                pubkey = %pubkey,
                                client_id = %client.id(),
                                timeout_ms = UNSUBSCRIBE_TIMEOUT.as_millis() as u64,
                                "Timed out dropping websocket leg for gRPC-only coverage"
                            );
                        }
                    }
                }),
        )
        .await;
        provisional_policy.commit();
        tombstone_guard.commit();
        #[cfg(test)]
        self.grpc_preference_completions
            .fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    fn revoke_grpc_subscription_preference(&self, pubkey: &Pubkey) {
        let _publication_operation = self
            .begin_publication_operation(PublicationDirtyKey::Account(*pubkey));
        self.grpc_only_subscriptions
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .remove(pubkey);
    }

    fn reinstate_grpc_subscription_preference(&self, pubkey: Pubkey) {
        let _publication_operation = self
            .begin_publication_operation(PublicationDirtyKey::Account(pubkey));
        self.unsubscribed_accounts
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .remove(&pubkey);
        self.grpc_only_subscriptions
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .insert(pubkey);
    }

    fn finalize_subscription_removal(&self, pubkey: &Pubkey) {
        let _publication_operation = self
            .begin_publication_operation(PublicationDirtyKey::Account(*pubkey));
        self.unsubscribed_accounts
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .remove(pubkey);
    }

    fn begin_account_subscription_publication(
        &self,
        pubkey: Pubkey,
        policy: AccountSubscriptionPublicationPolicy,
    ) -> Option<AccountSubscriptionPublicationToken> {
        let mut state = self
            .client_publication_state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let generation = state.next_generation();
        let token = AccountSubscriptionPublicationToken { pubkey, generation };
        state
            .active_operations
            .insert(generation, PublicationDirtyKey::Account(pubkey));
        state
            .provider_publications
            .entry(pubkey)
            .or_default()
            .push(PendingProviderPublication { token, policy });
        for attach in state.attaching_clients.values_mut() {
            attach.blockers.insert(generation);
        }
        state.mark_dirty(PublicationDirtyKey::Account(pubkey));
        Some(token)
    }

    fn update_account_subscription_publication(
        &self,
        token: AccountSubscriptionPublicationToken,
        policy: AccountSubscriptionPublicationPolicy,
    ) {
        let mut state = self
            .client_publication_state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let updated = state
            .provider_publications
            .get_mut(&token.pubkey)
            .and_then(|publications| {
                publications
                    .iter_mut()
                    .find(|publication| publication.token == token)
            })
            .map(|publication| publication.policy = policy)
            .is_some();
        if updated {
            state.mark_dirty(PublicationDirtyKey::Account(token.pubkey));
        }
    }

    fn finish_account_subscription_publication(
        &self,
        token: AccountSubscriptionPublicationToken,
        _outcome: AccountSubscriptionPublicationOutcome,
    ) {
        let mut state = self
            .client_publication_state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let mut removed = false;
        let mut remove_entry = false;
        if let Some(publications) =
            state.provider_publications.get_mut(&token.pubkey)
        {
            let before = publications.len();
            publications.retain(|publication| publication.token != token);
            removed = publications.len() != before;
            remove_entry = publications.is_empty();
        }
        if remove_entry {
            state.provider_publications.remove(&token.pubkey);
        }
        if removed {
            state.finish_operation(token.generation);
        }
    }

    fn mark_account_subscription_dirty(&self, pubkey: Pubkey) {
        self.client_publication_state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .mark_dirty(PublicationDirtyKey::Account(pubkey));
    }

    async fn shutdown(&self) -> RemoteAccountProviderResult<()> {
        AccountSubscriptionTask::Shutdown
            .process(self.clients_snapshot())
            .await
    }

    fn take_updates(&self) -> mpsc::Receiver<SubscriptionUpdate> {
        // Start forwarders on first take to ensure we have a consumer
        let out_rx = {
            let mut rx_lock = self.out_rx.lock().unwrap();
            // SAFETY: This can only be None if take_updates() is called more than once,
            // which indicates a logic bug by the caller. Panicking here surfaces the bug early.
            rx_lock
                .take()
                .expect("SubMuxClient::take_updates called more than once")
        };
        self.start_forwarders();
        out_rx
    }

    fn subscriptions_union(&self) -> HashSet<Pubkey> {
        let mut union = HashSet::new();
        for client in self.connected_clients_snapshot() {
            let subs = client.subscriptions_union();
            union.extend(subs);
        }
        union
    }

    fn is_subscribed(&self, pubkey: &Pubkey) -> bool {
        self.connected_clients_snapshot()
            .iter()
            .any(|client| client.is_subscribed(pubkey))
    }

    fn subscriptions_intersection(&self) -> HashSet<Pubkey> {
        let sets: Vec<HashSet<Pubkey>> = self
            .connected_clients_snapshot()
            .iter()
            .map(|c| c.subscriptions_intersection())
            .collect();
        if sets.is_empty() {
            return HashSet::new();
        }
        // Find the smallest set to iterate over, then check membership
        // in all others — no intermediate cloning/collecting.
        // SAFETY: we return above if the set is empty, so unwrap is safe here.
        let smallest = sets.iter().min_by_key(|s| s.len()).unwrap();
        smallest
            .iter()
            .filter(|pk| {
                sets.iter()
                    .filter(|s| !std::ptr::eq(*s, smallest))
                    .all(|s| s.contains(pk))
            })
            .copied()
            .collect()
    }

    fn subscription_reconciliation_snapshot(
        &self,
    ) -> Option<SubscriptionReconciliationSnapshot> {
        Self::reconciliation_snapshot_from_clients(
            self.connected_clients_snapshot(),
        )
    }

    fn subscription_reconciliation_snapshot_for_transport(
        &self,
        transport: PubsubTransport,
    ) -> Option<SubscriptionReconciliationSnapshot> {
        let clients = self
            .connected_clients_snapshot()
            .into_iter()
            .filter(|client| client.transport() == transport)
            .collect();
        Self::reconciliation_snapshot_from_clients(clients)
    }

    /// Returns true if any inner client subscribes immediately
    fn subs_immediately(&self) -> bool {
        self.clients_snapshot().iter().any(|c| c.subs_immediately())
    }

    fn id(&self) -> &str {
        "SubMuxClient"
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroUsize;

    use solana_account::Account;
    use tokio::sync::mpsc;

    use super::*;
    use crate::{
        remote_account_provider::{
            chain_pubsub_client::mock::ChainPubsubClientMock,
            subscription_reconciler::reconcile_subscriptions, AccountsLruCache,
        },
        submux::subscribed_accounts_tracker::mock::MockSubscribedAccountsTracker,
        testing::{init_logger, utils::sleep_ms},
    };

    fn account_with_lamports(lamports: u64) -> Account {
        Account {
            lamports,
            ..Account::default()
        }
    }
    fn new_submux_client(
        clients: Vec<Arc<ChainPubsubClientMock>>,
        dedupe_window_millis: Option<u64>,
    ) -> SubMuxClient<ChainPubsubClientMock> {
        let client_tuples = clients
            .into_iter()
            .map(|c| {
                let (_abort_tx, abort_rx) = mpsc::channel(1);
                (c, abort_rx)
            })
            .collect();
        let tracker = Arc::new(
            subscribed_accounts_tracker::mock::MockSubscribedAccountsTracker::new(
                vec![],
            ),
        );
        SubMuxClient::new(client_tuples, tracker, dedupe_window_millis)
    }

    fn new_submux_client_with_debounce(
        clients: Vec<Arc<ChainPubsubClientMock>>,
        config: DebounceConfig,
    ) -> SubMuxClient<ChainPubsubClientMock> {
        let client_tuples = clients
            .into_iter()
            .map(|c| {
                let (_abort_tx, abort_rx) = mpsc::channel(1);
                (c, abort_rx)
            })
            .collect();
        let tracker = Arc::new(
            subscribed_accounts_tracker::mock::MockSubscribedAccountsTracker::new(
                vec![],
            ),
        );
        SubMuxClient::new_with_debounce(client_tuples, tracker, config)
    }

    fn new_submux_with_abort(
        clients: Vec<Arc<ChainPubsubClientMock>>,
        subs: Vec<Pubkey>,
        dedupe_window_millis: Option<u64>,
    ) -> (SubMuxClient<ChainPubsubClientMock>, Vec<mpsc::Sender<()>>) {
        let mut abort_senders = Vec::new();
        let client_tuples = clients
            .into_iter()
            .map(|c| {
                let (abort_tx, abort_rx) = mpsc::channel(4);
                abort_senders.push(abort_tx);
                (c, abort_rx)
            })
            .collect();
        let tracker = Arc::new(MockSubscribedAccountsTracker::new(subs));
        (
            SubMuxClient::new(client_tuples, tracker, dedupe_window_millis),
            abort_senders,
        )
    }

    async fn wait_for_connected_clients(
        mux: &SubMuxClient<ChainPubsubClientMock>,
        expected: u16,
    ) {
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            let connected = mux.connected_clients.load(Ordering::SeqCst);
            let snapshot_len = mux.connected_clients_snapshot().len();
            if connected == expected && snapshot_len == expected as usize {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {expected} connected clients; \
                 connected_clients={connected}, snapshot_len={snapshot_len}"
            );
            sleep_ms(10).await;
        }
    }

    // -----------------
    // Subscribe/Unsubscribe
    // -----------------

    #[tokio::test]
    async fn test_submux_forwards_updates_from_multiple_clients() {
        init_logger();

        let (tx1, rx1) = mpsc::channel(10_000);
        let (tx2, rx2) = mpsc::channel(10_000);
        let client1 = Arc::new(ChainPubsubClientMock::new(tx1, rx1));
        let client2 = Arc::new(ChainPubsubClientMock::new(tx2, rx2));

        let mux: SubMuxClient<ChainPubsubClientMock> = new_submux_client(
            vec![client1.clone(), client2.clone()],
            Some(100),
        );

        // Both mock clients subscribe immediately, so counter should be initialized to 2
        assert_eq!(
            mux.connected_clients_subscribing_immediately
                .load(Ordering::SeqCst),
            2
        );
        // With 2 clients subscribing immediately:
        // - required_account_subscription_confirmations = max(1, (2 * 2) / 3) = max(1, 1) = 1
        // - required_program_subscription_confirmations = max(1, 2 / 3) = max(1, 0) = 1
        assert_eq!(mux.required_account_subscription_confirmations(), 1);
        assert_eq!(mux.required_program_subscription_confirmations(), 1);

        let mut mux_rx = mux.take_updates();

        let pk = Pubkey::new_unique();

        mux.subscribe(pk, None).await.unwrap();

        // send one update from each client
        client1
            .send_account_update(pk, 1, &account_with_lamports(10))
            .await;
        client2
            .send_account_update(pk, 2, &account_with_lamports(20))
            .await;

        // Expect to receive two updates (naive behavior)
        let u1 = tokio::time::timeout(
            std::time::Duration::from_millis(100),
            mux_rx.recv(),
        )
        .await
        .expect("first update expected")
        .expect("stream open");
        let u2 = tokio::time::timeout(
            std::time::Duration::from_millis(100),
            mux_rx.recv(),
        )
        .await
        .expect("second update expected")
        .expect("stream open");

        assert_eq!(u1.pubkey, pk);
        assert_eq!(u2.pubkey, pk);
        let lamports =
            |u: &SubscriptionUpdate| u.account.as_ref().unwrap().lamports;
        let mut lams = vec![lamports(&u1), lamports(&u2)];
        lams.sort();
        assert_eq!(lams, vec![10, 20]);

        mux.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn test_submux_add_client_resubscribes_and_forwards_updates() {
        init_logger();

        let pk = Pubkey::new_unique();
        let (tx1, rx1) = mpsc::channel(10_000);
        let (tx2, rx2) = mpsc::channel(10_000);
        let client1 = Arc::new(ChainPubsubClientMock::new(tx1, rx1));
        let client2 = Arc::new(ChainPubsubClientMock::new(tx2, rx2));
        let (_abort_tx1, abort_rx1) = mpsc::channel(1);
        let (_abort_tx2, abort_rx2) = mpsc::channel(1);
        let tracker = Arc::new(MockSubscribedAccountsTracker::new(vec![pk]));

        let mux: SubMuxClient<ChainPubsubClientMock> = SubMuxClient::new(
            vec![(client1, abort_rx1)],
            tracker.clone(),
            None,
        );
        let mut mux_rx = mux.take_updates();

        mux.add_client(client2.clone(), abort_rx2, tracker)
            .await
            .unwrap();

        assert!(client2.subscriptions_union().contains(&pk));
        client2
            .send_account_update(pk, 1, &account_with_lamports(42))
            .await;

        let update = tokio::time::timeout(
            std::time::Duration::from_millis(100),
            mux_rx.recv(),
        )
        .await
        .expect("update expected")
        .expect("stream open");
        assert_eq!(update.pubkey, pk);
        assert_eq!(update.account.unwrap().lamports, 42);

        mux.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn test_subscribe_program_retry_after_failure_reaches_client() {
        init_logger();

        let (tx, rx) = mpsc::channel(10_000);
        let client = Arc::new(ChainPubsubClientMock::new(tx, rx));
        client.fail_next_program_subscriptions(1);

        let mux: SubMuxClient<ChainPubsubClientMock> =
            new_submux_client(vec![client.clone()], Some(100));
        let program_id = Pubkey::new_unique();

        let err = mux
            .subscribe_program(program_id)
            .await
            .expect_err("first program subscription should fail");
        assert!(
            err.to_string().contains("forced program subscribe failure"),
            "unexpected error: {err}"
        );
        assert!(
            !mux.program_subs_lock().contains(&program_id),
            "failed program subscription must not be recorded"
        );
        assert_eq!(client.program_subscribe_attempts(), 1);

        mux.subscribe_program(program_id)
            .await
            .expect("retry should reach the client and succeed");

        assert_eq!(client.program_subscribe_attempts(), 2);
        assert!(mux.program_subs_lock().contains(&program_id));
        assert!(client.subscribed_program_ids().contains(&program_id));

        mux.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn test_failed_program_subscription_settles_before_queued_retry() {
        init_logger();

        let (tx, rx) = mpsc::channel(10_000);
        let client = Arc::new(ChainPubsubClientMock::new(tx, rx));
        client.block_program_subscribe();
        client.fail_next_program_subscriptions(1);
        let mux: SubMuxClient<ChainPubsubClientMock> =
            new_submux_client(vec![client.clone()], Some(100));
        let program_id = Pubkey::new_unique();

        let first_mux = mux.clone();
        let first = tokio::spawn(async move {
            first_mux.subscribe_program(program_id).await
        });
        client.wait_for_program_subscribe_attempts(1).await;
        let second_mux = mux.clone();
        let second = tokio::spawn(async move {
            second_mux.subscribe_program(program_id).await
        });

        client.release_program_subscribe();
        assert!(first.await.unwrap().is_err());
        second.await.unwrap().unwrap();
        assert_eq!(client.program_subscribe_attempts(), 2);
        assert!(mux.program_subs_lock().contains(&program_id));
        assert!(client.subscribed_program_ids().contains(&program_id));
        mux.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn test_cancelled_program_subscription_keeps_desired_state() {
        init_logger();

        let (tx, rx) = mpsc::channel(10_000);
        let client = Arc::new(ChainPubsubClientMock::new(tx, rx));
        client.block_program_subscribe();
        let mux: SubMuxClient<ChainPubsubClientMock> =
            new_submux_client(vec![client.clone()], Some(100));
        let program_id = Pubkey::new_unique();

        let task_mux = mux.clone();
        let subscribe = tokio::spawn(async move {
            task_mux.subscribe_program(program_id).await
        });
        client.wait_for_program_subscribe_attempts(1).await;
        assert!(mux.program_subs_lock().contains(&program_id));

        subscribe.abort();
        assert!(subscribe.await.unwrap_err().is_cancelled());
        client.release_program_subscribe();

        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            let settled = {
                let publication = mux.client_publication_state.lock().unwrap();
                !publication.active_operations.values().any(|dirty| {
                    matches!(
                        dirty,
                        PublicationDirtyKey::Program(id)
                            if *id == program_id
                    )
                })
            };
            if settled {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "cancelled program subscription did not settle"
            );
            tokio::task::yield_now().await;
        }

        assert!(mux.program_subs_lock().contains(&program_id));
        assert!(client.subscribed_program_ids().contains(&program_id));
        mux.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn test_failed_program_publication_does_not_leak_to_attach() {
        init_logger();

        let (tx1, rx1) = mpsc::channel(10_000);
        let (tx2, rx2) = mpsc::channel(10_000);
        let existing_client = Arc::new(ChainPubsubClientMock::new(tx1, rx1));
        let attaching_client = Arc::new(ChainPubsubClientMock::new(tx2, rx2));
        existing_client.block_program_subscribe();
        existing_client.fail_next_program_subscriptions(1);
        let (_abort_tx1, abort_rx1) = mpsc::channel(1);
        let (_abort_tx2, abort_rx2) = mpsc::channel(1);
        let tracker = Arc::new(MockSubscribedAccountsTracker::new(Vec::new()));
        let mux = SubMuxClient::new(
            vec![(existing_client.clone(), abort_rx1)],
            tracker.clone(),
            Some(100),
        );
        let program_id = Pubkey::new_unique();

        let task_mux = mux.clone();
        let subscribe = tokio::spawn(async move {
            task_mux.subscribe_program(program_id).await
        });
        existing_client.wait_for_program_subscribe_attempts(1).await;

        let task_mux = mux.clone();
        let task_tracker = tracker.clone();
        let task_client = attaching_client.clone();
        let attach = tokio::spawn(async move {
            task_mux
                .add_client(task_client, abort_rx2, task_tracker)
                .await
        });
        let attaching_key = SubMuxClient::<ChainPubsubClientMock>::client_key(
            &attaching_client,
        );
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            let processed_while_blocked = {
                let publication = mux.client_publication_state.lock().unwrap();
                let program_generation = publication
                    .active_operations
                    .iter()
                    .find_map(|(generation, dirty)| {
                        matches!(
                            dirty,
                            PublicationDirtyKey::Program(id)
                                if *id == program_id
                        )
                        .then_some(*generation)
                    });
                program_generation.is_some_and(|generation| {
                    publication
                        .attaching_clients
                        .get(&attaching_key)
                        .is_some_and(|attach| {
                            attach.blockers.contains(&generation)
                                && !attach.dirty_programs.contains(&program_id)
                        })
                })
            };
            if processed_while_blocked {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for attaching client to drain provisional program work"
            );
            tokio::task::yield_now().await;
        }
        assert_eq!(attaching_client.program_subscribe_attempts(), 0);

        existing_client.release_program_subscribe();
        assert!(subscribe.await.unwrap().is_err());
        attach.await.unwrap().unwrap();

        assert!(!mux.program_subs_lock().contains(&program_id));
        assert!(!existing_client
            .subscribed_program_ids()
            .contains(&program_id));
        assert!(!attaching_client
            .subscribed_program_ids()
            .contains(&program_id));
        mux.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn test_submux_unsubscribe_stops_forwarding() {
        init_logger();

        let (tx1, rx1) = mpsc::channel(10_000);
        let (tx2, rx2) = mpsc::channel(10_000);
        let client1 = Arc::new(ChainPubsubClientMock::new(tx1, rx1));
        let client2 = Arc::new(ChainPubsubClientMock::new(tx2, rx2));

        let mux: SubMuxClient<ChainPubsubClientMock> = new_submux_client(
            vec![client1.clone(), client2.clone()],
            Some(100),
        );
        let mut mux_rx = mux.take_updates();

        let pk = Pubkey::new_unique();

        mux.subscribe(pk, None).await.unwrap();

        client1
            .send_account_update(pk, 1, &account_with_lamports(1))
            .await;
        let _ = tokio::time::timeout(
            std::time::Duration::from_millis(100),
            mux_rx.recv(),
        )
        .await;

        // Unsubscribe and send again; should not receive within timeout
        mux.unsubscribe(pk).await.unwrap();
        client2
            .send_account_update(pk, 2, &account_with_lamports(2))
            .await;

        let recv = tokio::time::timeout(
            std::time::Duration::from_millis(500),
            mux_rx.recv(),
        )
        .await;
        assert!(recv.is_err(), "no update after unsubscribe");

        mux.shutdown().await.unwrap();
    }

    // -----------------
    // Dedupe
    // -----------------
    #[tokio::test]
    async fn test_submux_dedup_identical_slot_updates() {
        init_logger();

        let (tx1, rx1) = mpsc::channel(10_000);
        let (tx2, rx2) = mpsc::channel(10_000);
        let client1 = Arc::new(ChainPubsubClientMock::new(tx1, rx1));
        let client2 = Arc::new(ChainPubsubClientMock::new(tx2, rx2));

        let mux: SubMuxClient<ChainPubsubClientMock> = new_submux_client(
            vec![client1.clone(), client2.clone()],
            Some(100),
        );
        let mut mux_rx = mux.take_updates();

        let pk = Pubkey::new_unique();
        mux.subscribe(pk, None).await.unwrap();

        // Two updates with same pubkey and slot (slot=7) from different clients
        client1
            .send_account_update(pk, 7, &account_with_lamports(111))
            .await;
        client2
            .send_account_update(pk, 7, &account_with_lamports(111))
            .await;

        // Expect exactly one forwarded
        let first = tokio::time::timeout(
            std::time::Duration::from_millis(100),
            mux_rx.recv(),
        )
        .await
        .expect("first update expected")
        .expect("stream open");
        assert_eq!(first.pubkey, pk);
        assert_eq!(first.slot, 7);

        // No second within short timeout (dedup window is 2s)
        let recv = tokio::time::timeout(
            std::time::Duration::from_millis(400),
            mux_rx.recv(),
        )
        .await;
        assert!(recv.is_err(), "duplicate update should be deduped");

        // Now send a new slot; should pass through
        client1
            .send_account_update(pk, 8, &account_with_lamports(222))
            .await;
        let next = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            mux_rx.recv(),
        )
        .await
        .expect("next update expected")
        .expect("stream open");
        assert_eq!(next.slot, 8);

        mux.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn test_submux_dedup_multi_overlapping_within_window() {
        init_logger();

        let (tx1, rx1) = mpsc::channel(10_000);
        let (tx2, rx2) = mpsc::channel(10_000);
        let client1 = Arc::new(ChainPubsubClientMock::new(tx1, rx1));
        let client2 = Arc::new(ChainPubsubClientMock::new(tx2, rx2));

        let mux: SubMuxClient<ChainPubsubClientMock> = new_submux_client(
            vec![client1.clone(), client2.clone()],
            Some(100),
        );
        let mut mux_rx = mux.take_updates();

        let pk = Pubkey::new_unique();
        mux.subscribe(pk, None).await.unwrap();

        // Send updates within 100ms window: u1, u2, u1(again), u3, u2(again)
        client1
            .send_account_update(pk, 1, &account_with_lamports(11))
            .await;
        client1
            .send_account_update(pk, 2, &account_with_lamports(22))
            .await;
        client2
            .send_account_update(pk, 1, &account_with_lamports(11))
            .await;
        client2
            .send_account_update(pk, 3, &account_with_lamports(33))
            .await;
        client1
            .send_account_update(pk, 2, &account_with_lamports(22))
            .await;

        // Expect only three unique slots: 1, 2, 3
        let mut received = Vec::new();
        for _ in 0..3 {
            let up = tokio::time::timeout(
                std::time::Duration::from_millis(100),
                mux_rx.recv(),
            )
            .await
            .expect("expected update")
            .expect("stream open");
            received.push(up.slot);
        }
        received.sort_unstable();
        assert_eq!(received, vec![1, 2, 3]);

        // No further updates should arrive (duplicates were deduped)
        let recv_more = tokio::time::timeout(
            std::time::Duration::from_millis(100),
            mux_rx.recv(),
        )
        .await;
        assert!(recv_more.is_err(), "no extra updates expected");

        mux.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn test_submux_dedup_three_clients_with_delayed_fourth() {
        init_logger();

        let (tx1, rx1) = mpsc::channel(10_000);
        let (tx2, rx2) = mpsc::channel(10_000);
        let (tx3, rx3) = mpsc::channel(10_000);
        let client1 = Arc::new(ChainPubsubClientMock::new(tx1, rx1));
        let client2 = Arc::new(ChainPubsubClientMock::new(tx2, rx2));
        let client3 = Arc::new(ChainPubsubClientMock::new(tx3, rx3));

        let mux: SubMuxClient<ChainPubsubClientMock> = new_submux_client(
            vec![client1.clone(), client2.clone(), client3.clone()],
            Some(100),
        );
        let mut mux_rx = mux.take_updates();

        let pk = Pubkey::new_unique();
        mux.subscribe(pk, None).await.unwrap();

        // Within 100ms window
        client1
            .send_account_update(pk, 1, &account_with_lamports(1))
            .await;
        client1
            .send_account_update(pk, 2, &account_with_lamports(2))
            .await;
        client1
            .send_account_update(pk, 3, &account_with_lamports(3))
            .await;

        client2
            .send_account_update(pk, 2, &account_with_lamports(2))
            .await;
        client2
            .send_account_update(pk, 3, &account_with_lamports(3))
            .await;

        client3
            .send_account_update(pk, 1, &account_with_lamports(1))
            .await;
        client3
            .send_account_update(pk, 2, &account_with_lamports(2))
            .await;
        client3
            .send_account_update(pk, 3, &account_with_lamports(3))
            .await;

        // Expect only 1,2,3 once
        let mut first_batch = Vec::new();
        for _ in 0..3 {
            let up = tokio::time::timeout(
                std::time::Duration::from_millis(100),
                mux_rx.recv(),
            )
            .await
            .expect("expected first-batch update")
            .expect("stream open");
            first_batch.push(up.slot);
        }
        first_batch.sort_unstable();
        assert_eq!(first_batch, vec![1, 2, 3]);

        // Sleep just beyond dedupe window, then send update1 again
        sleep_ms(110).await;
        client2
            .send_account_update(pk, 1, &account_with_lamports(1))
            .await;

        // Expect update1 again
        let up = tokio::time::timeout(
            std::time::Duration::from_millis(100),
            mux_rx.recv(),
        )
        .await
        .expect("expected second-batch update")
        .expect("stream open");
        assert_eq!(up.slot, 1);

        mux.shutdown().await.unwrap();
    }

    // -----------------
    // Debounce
    // -----------------

    async fn send_schedule(
        client: Arc<ChainPubsubClientMock>,
        pk: Pubkey,
        base_lamports: u64,
        slots_and_delays: &[(u64, u64)],
    ) {
        // slots_and_delays contains (slot, target_delay_millis_from_previous_send)
        // We account for execution overhead by measuring the timestamp
        // when we actually send each update and sleeping only the
        // remaining time needed to match the requested delay.
        let mut last_sent_at: Option<Instant> = None;
        for (slot, delay_ms) in slots_and_delays {
            if let Some(sent_at) = last_sent_at {
                let desired = Duration::from_millis(*delay_ms);
                let elapsed = Instant::now().saturating_duration_since(sent_at);
                if desired > elapsed {
                    sleep_ms((desired - elapsed).as_millis() as u64).await;
                }
            }
            client
                .send_account_update(
                    pk,
                    *slot,
                    &account_with_lamports(base_lamports + *slot),
                )
                .await;
            // Capture the actual send timestamp for the next iteration
            last_sent_at = Some(Instant::now());
        }
    }

    async fn drain_slots(
        rx: &mut mpsc::Receiver<SubscriptionUpdate>,
        per_recv_timeout_ms: u64,
    ) -> Vec<u64> {
        let mut slots = Vec::new();
        while let Ok(Some(update)) = tokio::time::timeout(
            std::time::Duration::from_millis(per_recv_timeout_ms),
            rx.recv(),
        )
        .await
        {
            slots.push(update.slot);
        }
        slots
    }

    #[tokio::test]
    async fn test_debounce_fast_account() {
        init_logger();

        // Debounce interval 200ms, detection window 1000ms
        let (tx, rx) = mpsc::channel(10_000);
        let client = Arc::new(ChainPubsubClientMock::new(tx, rx));
        let mux: SubMuxClient<ChainPubsubClientMock> =
            new_submux_client_with_debounce(
                vec![client.clone()],
                DebounceConfig {
                    dedupe_window_millis: Some(100),
                    interval_millis: Some(200),
                    detection_window_millis: Some(1000),
                },
            );
        let mut mux_rx = mux.take_updates();
        let pk = Pubkey::new_unique();
        mux.subscribe(pk, None).await.unwrap();

        // A schedule adjusted to receive only indexes: 0,1,2,3,4,7,9
        // Explanation:
        // - 0..4 at +200ms to enable debouncing at index 4.
        // - 5:+100, 6:+50, 7:+40 all before the next_allowed_forward after 4;
        //   timer flush forwards 7 (dropping 5 and 6).
        // - 8:+110, 9:+90 both before the next_allowed_forward; flush forwards 9
        //   (dropping 8).
        let schedule: Vec<(u64, u64)> = vec![
            (0, 0),
            (1, 180),
            (2, 180),
            (3, 180),
            (4, 180),
            // Debounced
            (5, 100),
            (6, 50),
            (7, 40),
            (8, 100),
            // Forwarded by debounce flusher
            (9, 90),
        ];
        send_schedule(client.clone(), pk, 1000, &schedule).await;

        let mut received = drain_slots(&mut mux_rx, 800).await;
        received.sort_unstable();
        // With debounce interval equal to the inter-arrival times (200ms),
        // forwarding will allow one per interval. Thus we expect all slots.
        assert_eq!(received, vec![0, 1, 2, 3, 4, 7, 9]);

        let state = mux.get_debounce_state(pk).expect("debounce state for pk");

        assert!(
            state.arrivals_ref().len()
                <= mux.allowed_in_debounce_window_count()
        );

        mux.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn test_debounce_slow_account() {
        init_logger();

        let (tx, rx) = mpsc::channel(10_000);
        let client = Arc::new(ChainPubsubClientMock::new(tx, rx));
        let mux: SubMuxClient<ChainPubsubClientMock> =
            new_submux_client_with_debounce(
                vec![client.clone()],
                DebounceConfig {
                    dedupe_window_millis: Some(100),
                    interval_millis: Some(200),
                    detection_window_millis: Some(1000),
                },
            );
        let mut mux_rx = mux.take_updates();
        let pk = Pubkey::new_unique();
        mux.subscribe(pk, None).await.unwrap();

        // B (scaled): 00:0 | 01:+400 | 02:+400 | 03:+400 (never enters debounce)
        // Never debounced
        let schedule: Vec<(u64, u64)> =
            vec![(0, 0), (1, 400), (2, 400), (3, 400)];
        send_schedule(client.clone(), pk, 2000, &schedule).await;

        let received = drain_slots(&mut mux_rx, 800).await;
        assert_eq!(received, vec![0, 1, 2, 3]);

        let state = mux.get_debounce_state(pk).expect("debounce state for pk");
        assert!(
            state.arrivals_ref().len()
                <= mux.allowed_in_debounce_window_count()
        );

        mux.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn test_debounce_jittery_account() {
        init_logger();

        // Debounce interval 200ms, detection window 1000ms
        let (tx, rx) = mpsc::channel(10_000);
        let client = Arc::new(ChainPubsubClientMock::new(tx, rx));
        let mux: SubMuxClient<ChainPubsubClientMock> =
            new_submux_client_with_debounce(
                vec![client.clone()],
                DebounceConfig {
                    dedupe_window_millis: Some(100),
                    interval_millis: Some(200),
                    detection_window_millis: Some(1000),
                },
            );
        let mut mux_rx = mux.take_updates();
        let pk = Pubkey::new_unique();
        mux.subscribe(pk, None).await.unwrap();

        // Phases:
        // 1) First 5 updates at ~180ms: enables debounce on the 5th.
        // 2) Next 5 updates tightly spaced (40ms): only the last (slot 9) is sent.
        // 3) Long gap (1200ms) then 2 updates within window: disables debounce; both forwarded.
        // 4) Three low-frequency updates (400ms apart): all forwarded while disabled.
        let schedule: Vec<(u64, u64)> = vec![
            (0, 0),
            (1, 180),
            (2, 180),
            (3, 180),
            (4, 180),
            // Debounced
            (5, 30),
            (6, 30),
            (7, 30),
            (8, 30),
            // Forwarded by debounce flusher
            (9, 30),
            // Interval in the _allowed_ limit -> debounce disabled immediately
            // All the below updates forwarded immediately
            (10, 220),
            (11, 220),
            (12, 400),
            (13, 300),
        ];
        send_schedule(client.clone(), pk, 4000, &schedule).await;

        let mut received = drain_slots(&mut mux_rx, 800).await;
        received.sort_unstable();
        assert_eq!(received, vec![0, 1, 2, 3, 4, 9, 10, 11, 12, 13]);

        let state = mux.get_debounce_state(pk).expect("debounce state for pk");
        assert!(
            state.arrivals_ref().len()
                <= mux.allowed_in_debounce_window_count()
        );

        mux.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn test_sysvar_is_not_debounced() {
        init_logger();
        let (tx, rx) = mpsc::channel(10_000);
        let client = Arc::new(ChainPubsubClientMock::new(tx, rx));
        let mux: SubMuxClient<ChainPubsubClientMock> =
            new_submux_client_with_debounce(
                vec![client.clone()],
                DebounceConfig {
                    dedupe_window_millis: Some(100),
                    interval_millis: Some(200),
                    detection_window_millis: Some(1000),
                },
            );
        let mut mux_rx = mux.take_updates();

        // 1. Ensure that for another account's updates are debounced
        {
            let other = Pubkey::new_unique();
            mux.subscribe(other, None).await.unwrap();
            let schedule: Vec<(u64, u64)> = (0..10).map(|i| (i, 50)).collect();
            send_schedule(client.clone(), other, 5000, &schedule).await;
            let received = drain_slots(&mut mux_rx, 800).await;
            assert!(received.len() < 10, "some updates should be debounced");
        }

        // 2. Now subscribe to sysvar::clock and send same rapid updates
        //    None should be debounced
        {
            let clock = solana_program::sysvar::clock::ID;
            mux.subscribe(clock, None).await.unwrap();

            let schedule: Vec<(u64, u64)> = (0..10).map(|i| (i, 50)).collect();
            send_schedule(client.clone(), clock, 5000, &schedule).await;

            let received = drain_slots(&mut mux_rx, 800).await;
            assert_eq!(received.len(), 10, "no updates should be debounced");
        }

        mux.shutdown().await.unwrap();
    }

    // -----------------
    // Reconnection Tests
    // -----------------
    #[tokio::test]
    async fn test_reconnect_on_disconnect_reestablishes_subscriptions() {
        init_logger();

        let (tx1, rx1) = mpsc::channel(10_000);
        let (tx2, rx2) = mpsc::channel(10_000);
        let client1 = Arc::new(ChainPubsubClientMock::new(tx1, rx1));
        let client2 = Arc::new(ChainPubsubClientMock::new(tx2, rx2));

        let pk = Pubkey::new_unique();
        let (mux, aborts) = new_submux_with_abort(
            vec![client1.clone(), client2.clone()],
            vec![pk],
            Some(100),
        );

        // Initially both immediately subscribing clients are connected
        macro_rules! assert_all_clients_connected {
            () => {
                assert_eq!(
                    mux.connected_clients_subscribing_immediately
                        .load(Ordering::SeqCst),
                    2,
                    "Both clients should be connected initially"
                );
                assert_eq!(
                    mux.required_account_subscription_confirmations(),
                    1
                );
                assert_eq!(
                    mux.required_program_subscription_confirmations(),
                    1
                );
            };
        }
        assert_all_clients_connected!();

        let mut mux_rx = mux.take_updates();

        mux.subscribe(pk, None).await.unwrap();

        // Baseline: client1 update arrives
        client1
            .send_account_update(pk, 1, &account_with_lamports(111))
            .await;
        tokio::time::timeout(
            std::time::Duration::from_millis(200),
            mux_rx.recv(),
        )
        .await
        .expect("got baseline update")
        .expect("stream open");

        // Simulate disconnect: client1 loses subscriptions and is "disconnected"
        {
            client1.disable_reconnect();
            client1.simulate_disconnect();

            // Trigger reconnect via abort channel and wait for message to be processed
            aborts[0].send(()).await.expect("abort send");
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;

            // Only one direct sub client should be connected now (client2)
            assert_eq!(
                mux.connected_clients_subscribing_immediately
                    .load(Ordering::SeqCst),
                1
            );
            client1.enable_reconnect();

            // Wait for reconnect and resub to complete
            while !client1.is_connected_and_resubscribed() {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            let mut max_tries = 20;
            while mux
                .connected_clients_subscribing_immediately
                .load(Ordering::SeqCst)
                < 2
                && max_tries > 0
            {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                max_tries -= 1;
            }
        }

        // After reconnect, client1 should be connected again
        assert_all_clients_connected!();

        // After reconnect + resubscribe, client1's updates should be forwarded again
        client1
            .send_account_update(pk, 2, &account_with_lamports(222))
            .await;

        let up = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            mux_rx.recv(),
        )
        .await
        .expect("expect update after reconnect")
        .expect("stream open");
        assert_eq!(up.pubkey, pk);
        assert_eq!(up.slot, 2);

        mux.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn test_reconnect_after_failed_resubscription_eventually_recovers() {
        init_logger();

        let (tx1, rx1) = mpsc::channel(10_000);
        let (tx2, rx2) = mpsc::channel(10_000);
        let client1 = Arc::new(ChainPubsubClientMock::new(tx1, rx1));
        let client2 = Arc::new(ChainPubsubClientMock::new(tx2, rx2));

        let pk = Pubkey::new_unique();
        let (mux, aborts) = new_submux_with_abort(
            vec![client1.clone(), client2.clone()],
            vec![pk],
            Some(100),
        );
        let mut mux_rx = mux.take_updates();

        mux.subscribe(pk, None).await.unwrap();

        // Prepare: first resubscribe attempt will fail
        client1.fail_next_resubscriptions(1);

        // Simulate disconnect: client1 loses subs and is disconnected
        client1.simulate_disconnect();

        // Trigger reconnect; first attempt will fail resub; reconnector will retry after ~1s (fib(1)=1)
        aborts[0].send(()).await.expect("abort send");

        // Send updates until one passes after reconnection and resubscribe succeed
        // Keep unique slots to avoid dedupe
        let mut slot: u64 = 100;
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut got = None;
        while Instant::now() < deadline {
            client1
                .send_account_update(
                    pk,
                    slot,
                    &account_with_lamports(1_000 + slot),
                )
                .await;
            if let Ok(Some(u)) = tokio::time::timeout(
                std::time::Duration::from_millis(200),
                mux_rx.recv(),
            )
            .await
            {
                got = Some(u);
                break;
            }
            slot += 1;
        }

        let up = got.expect("should receive update after retry reconnect");
        assert_eq!(up.pubkey, pk);
        assert!(up.slot >= 100);

        mux.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn test_reconciliation_snapshots_ignore_reconnecting_clients() {
        init_logger();

        let (tx1, rx1) = mpsc::channel(10_000);
        let (tx2, rx2) = mpsc::channel(10_000);
        let (tx3, rx3) = mpsc::channel(10_000);
        let client1 = Arc::new(ChainPubsubClientMock::new(tx1, rx1));
        let client2 = Arc::new(ChainPubsubClientMock::new(tx2, rx2));
        let client3 = Arc::new(ChainPubsubClientMock::new(tx3, rx3));

        let shared_pk = Pubkey::new_unique();
        let reconnecting_only_pk = Pubkey::new_unique();
        let (mux, aborts) = new_submux_with_abort(
            vec![client1.clone(), client2.clone(), client3.clone()],
            vec![shared_pk],
            Some(100),
        );

        mux.subscribe(shared_pk, None).await.unwrap();
        assert!(client1.subscriptions_union().contains(&shared_pk));
        assert!(client2.subscriptions_union().contains(&shared_pk));
        assert!(client3.subscriptions_union().contains(&shared_pk));

        client3.disable_reconnect();
        client3.simulate_disconnect();
        client3.insert_subscription(reconnecting_only_pk);
        aborts[2].send(()).await.expect("abort send");
        wait_for_connected_clients(&mux, 2).await;

        assert_eq!(mux.connected_clients.load(Ordering::SeqCst), 2);
        assert_eq!(mux.connected_clients_snapshot().len(), 2);

        let intersection = mux.subscriptions_intersection();
        assert!(
            intersection.contains(&shared_pk),
            "connected clients still agree on the shared subscription"
        );
        assert!(
            !intersection.contains(&reconnecting_only_pk),
            "reconnecting-only subscriptions must not affect intersection"
        );

        let union = mux.subscriptions_union();
        assert!(
            union.contains(&shared_pk),
            "connected-client union should include shared subscriptions"
        );
        assert!(
            !union.contains(&reconnecting_only_pk),
            "reconnecting-only subscriptions must not affect union"
        );

        mux.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn test_prefer_grpc_subscription_drops_ws_legs() {
        init_logger();

        let (tx1, rx1) = mpsc::channel(10_000);
        let (tx2, rx2) = mpsc::channel(10_000);
        let ws_client = Arc::new(ChainPubsubClientMock::new(tx1, rx1));
        let grpc_client = Arc::new(ChainPubsubClientMock::new(tx2, rx2));
        grpc_client.set_transport(PubsubTransport::Grpc);

        let pk = Pubkey::new_unique();
        let (mux, _aborts) = new_submux_with_abort(
            vec![ws_client.clone(), grpc_client.clone()],
            vec![pk],
            Some(100),
        );

        mux.subscribe(pk, None).await.unwrap();
        assert!(ws_client.subscriptions_union().contains(&pk));
        assert!(grpc_client.subscriptions_union().contains(&pk));

        mux.prefer_grpc_subscription(pk).await.unwrap();
        assert!(!ws_client.subscriptions_union().contains(&pk));
        assert!(grpc_client.subscriptions_union().contains(&pk));
        assert_eq!(ws_client.unsubscribe_attempts(), 1);

        mux.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn test_prefer_grpc_subscription_initial_admission_is_idempotent() {
        init_logger();

        let (tx1, rx1) = mpsc::channel(10_000);
        let (tx2, rx2) = mpsc::channel(10_000);
        let (tx3, rx3) = mpsc::channel(10_000);
        let ws_client = Arc::new(ChainPubsubClientMock::new(tx1, rx1));
        let grpc_client1 = Arc::new(ChainPubsubClientMock::new(tx2, rx2));
        let grpc_client2 = Arc::new(ChainPubsubClientMock::new(tx3, rx3));
        grpc_client1.set_transport(PubsubTransport::Grpc);
        grpc_client2.set_transport(PubsubTransport::Grpc);

        let pk = Pubkey::new_unique();
        let (mux, _aborts) = new_submux_with_abort(
            vec![
                ws_client.clone(),
                grpc_client1.clone(),
                grpc_client2.clone(),
            ],
            vec![pk],
            Some(100),
        );

        mux.prefer_grpc_subscription(pk).await.unwrap();
        assert_eq!(ws_client.subscribe_attempts(), 0);
        assert_eq!(ws_client.unsubscribe_attempts(), 0);
        assert_eq!(grpc_client1.subscribe_attempts(), 1);
        assert_eq!(grpc_client2.subscribe_attempts(), 1);
        assert!(!ws_client.subscriptions_union().contains(&pk));
        assert!(grpc_client1.subscriptions_union().contains(&pk));
        assert!(grpc_client2.subscriptions_union().contains(&pk));

        mux.prefer_grpc_subscription(pk).await.unwrap();
        assert_eq!(ws_client.subscribe_attempts(), 0);
        assert_eq!(ws_client.unsubscribe_attempts(), 0);
        assert_eq!(grpc_client1.subscribe_attempts(), 1);
        assert_eq!(grpc_client2.subscribe_attempts(), 1);

        // A partial gRPC drift repairs only the missing client.
        grpc_client2.remove_subscription(&pk);
        mux.prefer_grpc_subscription(pk).await.unwrap();
        assert_eq!(grpc_client1.subscribe_attempts(), 1);
        assert_eq!(grpc_client2.subscribe_attempts(), 2);
        assert_eq!(ws_client.subscribe_attempts(), 0);
        assert_eq!(ws_client.unsubscribe_attempts(), 0);

        mux.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn test_grpc_only_policy_covers_client_attaching_before_tracker_publication(
    ) {
        init_logger();

        let (tx1, rx1) = mpsc::channel(10_000);
        let (tx2, rx2) = mpsc::channel(10_000);
        let (tx3, rx3) = mpsc::channel(10_000);
        let ws_client = Arc::new(ChainPubsubClientMock::new(tx1, rx1));
        let grpc_client1 = Arc::new(ChainPubsubClientMock::new(tx2, rx2));
        let grpc_client2 = Arc::new(ChainPubsubClientMock::new(tx3, rx3));
        grpc_client1.set_transport(PubsubTransport::Grpc);
        grpc_client2.set_transport(PubsubTransport::Grpc);
        let (_abort_tx1, abort_rx1) = mpsc::channel(1);
        let (_abort_tx2, abort_rx2) = mpsc::channel(1);
        let (_abort_tx3, abort_rx3) = mpsc::channel(1);
        let tracker = Arc::new(MockSubscribedAccountsTracker::new(Vec::new()));
        let mux = SubMuxClient::new(
            vec![
                (ws_client.clone(), abort_rx1),
                (grpc_client1.clone(), abort_rx2),
            ],
            tracker.clone(),
            Some(100),
        );

        let pubkey = Pubkey::new_unique();
        mux.prefer_grpc_subscription(pubkey).await.unwrap();
        assert!(tracker.subscribed_accounts().is_empty());
        assert!(grpc_client1.is_subscribed(&pubkey));
        assert!(!ws_client.is_subscribed(&pubkey));

        // Model a client attaching after transport admission but before the
        // provider publishes the key to its secondary LRU.
        mux.add_client(grpc_client2.clone(), abort_rx3, tracker)
            .await
            .unwrap();
        assert!(grpc_client2.is_subscribed(&pubkey));

        grpc_client1.remove_subscription(&pubkey);
        assert!(mux.is_subscribed(&pubkey));
        mux.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn test_attach_waits_for_provider_publication_and_rechecks_abort() {
        init_logger();

        let (tx1, rx1) = mpsc::channel(10_000);
        let (tx2, rx2) = mpsc::channel(10_000);
        let existing_client = Arc::new(ChainPubsubClientMock::new(tx1, rx1));
        let attaching_client = Arc::new(ChainPubsubClientMock::new(tx2, rx2));
        let (_abort_tx1, abort_rx1) = mpsc::channel(1);
        let (_abort_tx2, abort_rx2) = mpsc::channel(1);
        let tracker = Arc::new(MockSubscribedAccountsTracker::new(Vec::new()));
        let mux = SubMuxClient::new(
            vec![(existing_client, abort_rx1)],
            tracker.clone(),
            Some(100),
        );
        let pubkey = Pubkey::new_unique();
        let token = mux
            .begin_account_subscription_publication(
                pubkey,
                AccountSubscriptionPublicationPolicy::Full,
            )
            .expect("submux should provide a publication token");

        let task_mux = mux.clone();
        let task_tracker = tracker.clone();
        let task_client = attaching_client.clone();
        let attach = tokio::spawn(async move {
            task_mux
                .add_client(task_client, abort_rx2, task_tracker)
                .await
        });

        attaching_client.wait_for_subscribe_attempts(2).await;
        let attaching_key = SubMuxClient::<ChainPubsubClientMock>::client_key(
            &attaching_client,
        );
        {
            let publication = mux.client_publication_state.lock().unwrap();
            let attaching_state = publication
                .attaching_clients
                .get(&attaching_key)
                .expect("client must remain in attach publication");
            assert!(attaching_state.blockers.contains(&token.generation));
        }
        assert_eq!(mux.connected_clients_snapshot().len(), 1);

        mux.finish_account_subscription_publication(
            token,
            AccountSubscriptionPublicationOutcome::Aborted,
        );
        tokio::time::timeout(Duration::from_secs(1), attach)
            .await
            .expect("attach should wake after publication abort")
            .expect("attach task should not panic")
            .expect("attach should catch up to authoritative state");

        assert_eq!(mux.connected_clients_snapshot().len(), 2);
        assert!(
            !attaching_client.is_subscribed(&pubkey),
            "aborted provisional subscription must be removed before publish"
        );
        mux.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn test_cancelled_grpc_first_admission_rolls_back_provisional_policy()
    {
        init_logger();

        let (tx1, rx1) = mpsc::channel(10_000);
        let (tx2, rx2) = mpsc::channel(10_000);
        let ws_client = Arc::new(ChainPubsubClientMock::new(tx1, rx1));
        let grpc_client = Arc::new(ChainPubsubClientMock::new(tx2, rx2));
        grpc_client.set_transport(PubsubTransport::Grpc);
        grpc_client.block_subscribe();

        let pubkey = Pubkey::new_unique();
        let (mux, _aborts) = new_submux_with_abort(
            vec![ws_client, grpc_client.clone()],
            Vec::new(),
            Some(100),
        );
        let task_mux = mux.clone();
        let admission = tokio::spawn(async move {
            task_mux.prefer_grpc_subscription(pubkey).await
        });
        grpc_client.wait_for_subscribe_attempts(1).await;
        assert!(mux
            .grpc_only_subscriptions
            .lock()
            .unwrap()
            .contains(&pubkey));

        admission.abort();
        let _ = admission.await;
        assert!(!mux
            .grpc_only_subscriptions
            .lock()
            .unwrap()
            .contains(&pubkey));
        mux.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn test_failed_final_unsubscribe_revokes_grpc_reconnect_authority() {
        init_logger();

        let (tx1, rx1) = mpsc::channel(10_000);
        let (tx2, rx2) = mpsc::channel(10_000);
        let grpc_client1 = Arc::new(ChainPubsubClientMock::new(tx1, rx1));
        let grpc_client2 = Arc::new(ChainPubsubClientMock::new(tx2, rx2));
        grpc_client1.set_transport(PubsubTransport::Grpc);
        grpc_client2.set_transport(PubsubTransport::Grpc);
        let (_abort_tx1, abort_rx1) = mpsc::channel(1);
        let (_abort_tx2, abort_rx2) = mpsc::channel(1);

        let pubkey = Pubkey::new_unique();
        let tracker =
            Arc::new(MockSubscribedAccountsTracker::new(vec![pubkey]));
        let mux = SubMuxClient::new(
            vec![(grpc_client1.clone(), abort_rx1)],
            tracker.clone(),
            Some(100),
        );
        mux.prefer_grpc_subscription(pubkey).await.unwrap();
        tracker.set_subscriptions(Vec::new());

        // Final-removal callers revoke desired reconnect state before
        // attempting best-effort transport cleanup.
        mux.revoke_grpc_subscription_preference(&pubkey);
        grpc_client1.fail_next_unsubscriptions(1);
        assert!(mux.unsubscribe(pubkey).await.is_err());
        assert!(!mux
            .grpc_only_subscriptions
            .lock()
            .unwrap()
            .contains(&pubkey));

        // A later attach uses the same authority as reconnect. The failed
        // transport cleanup must not make the evicted key desired again.
        mux.add_client(grpc_client2.clone(), abort_rx2, tracker)
            .await
            .unwrap();
        assert!(!grpc_client2.is_subscribed(&pubkey));
        mux.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn test_failed_unsubscribe_restores_grpc_reconnect_authority() {
        init_logger();

        let (tx1, rx1) = mpsc::channel(10_000);
        let (tx2, rx2) = mpsc::channel(10_000);
        let grpc_client = Arc::new(ChainPubsubClientMock::new(tx1, rx1));
        let websocket_client = Arc::new(ChainPubsubClientMock::new(tx2, rx2));
        grpc_client.set_transport(PubsubTransport::Grpc);
        let (_abort_tx1, abort_rx1) = mpsc::channel(1);
        let (_abort_tx2, abort_rx2) = mpsc::channel(1);

        let pubkey = Pubkey::new_unique();
        let tracker =
            Arc::new(MockSubscribedAccountsTracker::new(vec![pubkey]));
        let mux = SubMuxClient::new(
            vec![(grpc_client.clone(), abort_rx1)],
            tracker.clone(),
            Some(100),
        );
        mux.prefer_grpc_subscription(pubkey).await.unwrap();

        grpc_client.fail_next_unsubscriptions(1);
        assert!(mux.unsubscribe(pubkey).await.is_err());
        assert!(mux
            .grpc_only_subscriptions
            .lock()
            .unwrap()
            .contains(&pubkey));

        // Membership remains authoritative after failed cleanup, so a later
        // websocket attachment must still honor the gRPC-only policy.
        mux.add_client(websocket_client.clone(), abort_rx2, tracker)
            .await
            .unwrap();
        assert!(!websocket_client.is_subscribed(&pubkey));
        mux.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn test_failed_unsubscribe_settles_before_queued_full_subscribe() {
        init_logger();

        let (tx1, rx1) = mpsc::channel(10_000);
        let (tx2, rx2) = mpsc::channel(10_000);
        let grpc_client = Arc::new(ChainPubsubClientMock::new(tx1, rx1));
        let websocket_client = Arc::new(ChainPubsubClientMock::new(tx2, rx2));
        grpc_client.set_transport(PubsubTransport::Grpc);
        let pubkey = Pubkey::new_unique();
        let (mux, _aborts) = new_submux_with_abort(
            vec![websocket_client.clone(), grpc_client.clone()],
            vec![pubkey],
            Some(100),
        );
        mux.subscribe(pubkey, None).await.unwrap();
        mux.prefer_grpc_subscription(pubkey).await.unwrap();
        assert!(mux
            .grpc_only_subscriptions
            .lock()
            .unwrap()
            .contains(&pubkey));

        let grpc_unsubscribe_attempts = grpc_client.unsubscribe_attempts();
        let websocket_unsubscribe_attempts =
            websocket_client.unsubscribe_attempts();
        grpc_client.block_unsubscribe();
        websocket_client.block_unsubscribe();
        grpc_client.fail_next_unsubscriptions(1);
        websocket_client.fail_next_unsubscriptions(1);
        let first_mux = mux.clone();
        let unsubscribe =
            tokio::spawn(async move { first_mux.unsubscribe(pubkey).await });
        grpc_client
            .wait_for_unsubscribe_attempts(grpc_unsubscribe_attempts + 1)
            .await;
        websocket_client
            .wait_for_unsubscribe_attempts(websocket_unsubscribe_attempts + 1)
            .await;

        let second_mux = mux.clone();
        let subscribe =
            tokio::spawn(
                async move { second_mux.subscribe(pubkey, None).await },
            );
        grpc_client.release_unsubscribe();
        websocket_client.release_unsubscribe();

        assert!(unsubscribe.await.unwrap().is_err());
        subscribe.await.unwrap().unwrap();
        assert!(!mux
            .grpc_only_subscriptions
            .lock()
            .unwrap()
            .contains(&pubkey));
        assert!(!mux.unsubscribed_accounts.lock().unwrap().contains(&pubkey));
        assert!(grpc_client.is_subscribed(&pubkey));
        assert!(websocket_client.is_subscribed(&pubkey));
        mux.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn test_cancelled_full_coverage_restore_rolls_back_grpc_only_policy()
    {
        init_logger();

        let (tx1, rx1) = mpsc::channel(10_000);
        let (tx2, rx2) = mpsc::channel(10_000);
        let websocket_client = Arc::new(ChainPubsubClientMock::new(tx1, rx1));
        let grpc_client = Arc::new(ChainPubsubClientMock::new(tx2, rx2));
        grpc_client.set_transport(PubsubTransport::Grpc);

        let pubkey = Pubkey::new_unique();
        let (mux, _aborts) = new_submux_with_abort(
            vec![websocket_client.clone(), grpc_client.clone()],
            vec![pubkey],
            Some(100),
        );
        mux.prefer_grpc_subscription(pubkey).await.unwrap();
        websocket_client.block_subscribe();
        grpc_client.block_subscribe();
        let ws_attempts = websocket_client.subscribe_attempts();
        let grpc_attempts = grpc_client.subscribe_attempts();

        let task_mux = mux.clone();
        let restore =
            tokio::spawn(async move { task_mux.subscribe(pubkey, None).await });
        websocket_client
            .wait_for_subscribe_attempts(ws_attempts + 1)
            .await;
        grpc_client
            .wait_for_subscribe_attempts(grpc_attempts + 1)
            .await;
        assert!(!mux
            .grpc_only_subscriptions
            .lock()
            .unwrap()
            .contains(&pubkey));

        restore.abort();
        assert!(restore.await.unwrap_err().is_cancelled());
        websocket_client.release_subscribe();
        grpc_client.release_subscribe();
        let deadline = Instant::now() + Duration::from_secs(1);
        while !mux
            .grpc_only_subscriptions
            .lock()
            .unwrap()
            .contains(&pubkey)
        {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for cancelled full restore to roll back"
            );
            sleep_ms(10).await;
        }
        mux.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn test_prefer_grpc_subscription_keeps_ws_on_partial_grpc_failure() {
        init_logger();

        let (tx1, rx1) = mpsc::channel(10_000);
        let (tx2, rx2) = mpsc::channel(10_000);
        let (tx3, rx3) = mpsc::channel(10_000);
        let ws_client = Arc::new(ChainPubsubClientMock::new(tx1, rx1));
        let grpc_client1 = Arc::new(ChainPubsubClientMock::new(tx2, rx2));
        let grpc_client2 = Arc::new(ChainPubsubClientMock::new(tx3, rx3));
        grpc_client1.set_transport(PubsubTransport::Grpc);
        grpc_client2.set_transport(PubsubTransport::Grpc);

        let pubkey = Pubkey::new_unique();
        let (mux, _aborts) = new_submux_with_abort(
            vec![ws_client.clone(), grpc_client1, grpc_client2.clone()],
            vec![pubkey],
            Some(100),
        );
        mux.subscribe(pubkey, None).await.unwrap();

        grpc_client2.remove_subscription(&pubkey);
        grpc_client2.simulate_disconnect();
        let ws_unsubscribe_attempts = ws_client.unsubscribe_attempts();
        assert!(mux.prefer_grpc_subscription(pubkey).await.is_err());
        assert!(ws_client.subscriptions_union().contains(&pubkey));
        assert_eq!(ws_client.unsubscribe_attempts(), ws_unsubscribe_attempts);

        mux.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn test_grpc_only_subscription_is_restored_on_grpc_reconnect() {
        init_logger();

        let (tx1, rx1) = mpsc::channel(10_000);
        let (tx2, rx2) = mpsc::channel(10_000);
        let ws_client = Arc::new(ChainPubsubClientMock::new(tx1, rx1));
        let grpc_client = Arc::new(ChainPubsubClientMock::new(tx2, rx2));
        grpc_client.set_transport(PubsubTransport::Grpc);

        let pubkey = Pubkey::new_unique();
        let (mux, aborts) = new_submux_with_abort(
            vec![ws_client.clone(), grpc_client.clone()],
            vec![pubkey],
            Some(100),
        );

        mux.subscribe(pubkey, None).await.unwrap();
        mux.prefer_grpc_subscription(pubkey).await.unwrap();
        grpc_client.simulate_disconnect();
        aborts[1].send(()).await.unwrap();

        let deadline = Instant::now() + Duration::from_secs(1);
        while !grpc_client.is_subscribed(&pubkey)
            || ws_client.is_subscribed(&pubkey)
        {
            assert!(
                Instant::now() < deadline,
                "gRPC-only policy did not recover"
            );
            sleep_ms(10).await;
        }

        mux.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn test_grpc_disconnect_restores_ws_fallback_until_reconnect() {
        init_logger();

        let (tx1, rx1) = mpsc::channel(10_000);
        let (tx2, rx2) = mpsc::channel(10_000);
        let ws_client = Arc::new(ChainPubsubClientMock::new(tx1, rx1));
        let grpc_client = Arc::new(ChainPubsubClientMock::new(tx2, rx2));
        grpc_client.set_transport(PubsubTransport::Grpc);

        let pubkey = Pubkey::new_unique();
        let (mux, aborts) = new_submux_with_abort(
            vec![ws_client.clone(), grpc_client.clone()],
            vec![pubkey],
            Some(100),
        );

        mux.subscribe(pubkey, None).await.unwrap();
        mux.prefer_grpc_subscription(pubkey).await.unwrap();
        assert!(!ws_client.is_subscribed(&pubkey));

        grpc_client.disable_reconnect();
        grpc_client.simulate_disconnect();
        aborts[1].send(()).await.unwrap();
        let fallback_deadline = Instant::now() + Duration::from_secs(1);
        while !ws_client.is_subscribed(&pubkey) {
            assert!(
                Instant::now() < fallback_deadline,
                "websocket fallback did not recover after gRPC disconnect"
            );
            sleep_ms(10).await;
        }

        grpc_client.enable_reconnect();
        let reconnect_deadline = Instant::now() + Duration::from_secs(2);
        while !grpc_client.is_subscribed(&pubkey)
            || ws_client.is_subscribed(&pubkey)
        {
            assert!(
                Instant::now() < reconnect_deadline,
                "gRPC reconnect did not restore gRPC-only policy"
            );
            sleep_ms(10).await;
        }

        mux.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn test_ws_reconnect_restores_key_when_grpc_coverage_is_partial() {
        init_logger();

        let (tx1, rx1) = mpsc::channel(10_000);
        let (tx2, rx2) = mpsc::channel(10_000);
        let (tx3, rx3) = mpsc::channel(10_000);
        let ws_client = Arc::new(ChainPubsubClientMock::new(tx1, rx1));
        let grpc_client1 = Arc::new(ChainPubsubClientMock::new(tx2, rx2));
        let grpc_client2 = Arc::new(ChainPubsubClientMock::new(tx3, rx3));
        grpc_client1.set_transport(PubsubTransport::Grpc);
        grpc_client2.set_transport(PubsubTransport::Grpc);

        let pubkey = Pubkey::new_unique();
        let (mux, aborts) = new_submux_with_abort(
            vec![
                ws_client.clone(),
                grpc_client1.clone(),
                grpc_client2.clone(),
            ],
            vec![pubkey],
            Some(100),
        );
        mux.prefer_grpc_subscription(pubkey).await.unwrap();

        // Keep both gRPC clients connected, but drift one subscription out
        // of coverage. A reconnecting websocket must restore the key even
        // though the other gRPC client still covers it.
        grpc_client2.remove_subscription(&pubkey);

        ws_client.simulate_disconnect();
        aborts[0].send(()).await.unwrap();
        let deadline = Instant::now() + Duration::from_secs(1);
        while !ws_client.subscriptions_union().contains(&pubkey) {
            assert!(
                Instant::now() < deadline,
                "websocket did not restore uncovered gRPC-only subscription"
            );
            sleep_ms(10).await;
        }

        mux.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn test_reconcile_replaces_lingering_ws_with_grpc_coverage() {
        init_logger();

        let (tx1, rx1) = mpsc::channel(10_000);
        let (tx2, rx2) = mpsc::channel(10_000);
        let ws_client = Arc::new(ChainPubsubClientMock::new(tx1, rx1));
        let grpc_client = Arc::new(ChainPubsubClientMock::new(tx2, rx2));
        grpc_client.set_transport(PubsubTransport::Grpc);

        let pubkey = Pubkey::new_unique();
        let (mux, _aborts) = new_submux_with_abort(
            vec![ws_client.clone(), grpc_client.clone()],
            vec![pubkey],
            Some(100),
        );
        ws_client.insert_subscription(pubkey);

        let primary = AccountsLruCache::new(NonZeroUsize::new(10).unwrap());
        let secondary = AccountsLruCache::new(NonZeroUsize::new(10).unwrap());
        secondary.add(pubkey);
        let (removed_tx, mut removed_rx) = mpsc::channel(10);

        reconcile_subscriptions(
            &primary,
            &secondary,
            &mux,
            &[],
            &removed_tx,
            None,
            None,
            None,
            None,
            None,
        )
        .await;

        assert!(!ws_client.subscriptions_union().contains(&pubkey));
        assert!(grpc_client.subscriptions_union().contains(&pubkey));
        assert!(removed_rx.try_recv().is_err());
        mux.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn test_reconcile_moves_pending_secondary_to_grpc_only() {
        init_logger();

        let (tx1, rx1) = mpsc::channel(10_000);
        let (tx2, rx2) = mpsc::channel(10_000);
        let ws_client = Arc::new(ChainPubsubClientMock::new(tx1, rx1));
        let grpc_client = Arc::new(ChainPubsubClientMock::new(tx2, rx2));
        grpc_client.set_transport(PubsubTransport::Grpc);

        let pubkey = Pubkey::new_unique();
        let (mux, _aborts) = new_submux_with_abort(
            vec![ws_client.clone(), grpc_client.clone()],
            vec![pubkey],
            Some(100),
        );
        ws_client.insert_subscription(pubkey);

        let primary = AccountsLruCache::new(NonZeroUsize::new(10).unwrap());
        let secondary = AccountsLruCache::new(NonZeroUsize::new(10).unwrap());
        secondary.add(pubkey);
        let (removed_tx, mut removed_rx) = mpsc::channel(10);

        reconcile_subscriptions(
            &primary,
            &secondary,
            &mux,
            &[],
            &removed_tx,
            None,
            None,
            None,
            None,
            None,
        )
        .await;

        assert!(!ws_client.subscriptions_union().contains(&pubkey));
        assert!(grpc_client.subscriptions_union().contains(&pubkey));
        assert!(removed_rx.try_recv().is_err());
        mux.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn test_prefer_grpc_subscription_without_grpc_keeps_ws_legs() {
        init_logger();

        let (tx1, rx1) = mpsc::channel(10_000);
        let ws_client = Arc::new(ChainPubsubClientMock::new(tx1, rx1));

        let pk = Pubkey::new_unique();
        let (mux, _aborts) =
            new_submux_with_abort(vec![ws_client.clone()], vec![pk], Some(100));

        mux.subscribe(pk, None).await.unwrap();
        assert!(mux.prefer_grpc_subscription(pk).await.is_err());
        assert!(ws_client.subscriptions_union().contains(&pk));
        assert!(!mux.grpc_only_subscriptions.lock().unwrap().contains(&pk));

        mux.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn test_reconcile_skips_repair_when_no_submux_clients_connected() {
        init_logger();

        let (tx1, rx1) = mpsc::channel(10_000);
        let (tx2, rx2) = mpsc::channel(10_000);
        let client1 = Arc::new(ChainPubsubClientMock::new(tx1, rx1));
        let client2 = Arc::new(ChainPubsubClientMock::new(tx2, rx2));

        let pk = Pubkey::new_unique();
        let (mux, aborts) = new_submux_with_abort(
            vec![client1.clone(), client2.clone()],
            vec![pk],
            Some(100),
        );

        mux.subscribe(pk, None).await.unwrap();

        client1.disable_reconnect();
        client2.disable_reconnect();
        client1.simulate_disconnect();
        client2.simulate_disconnect();
        aborts[0].send(()).await.expect("abort send 1");
        aborts[1].send(()).await.expect("abort send 2");
        wait_for_connected_clients(&mux, 0).await;

        assert_eq!(mux.connected_clients.load(Ordering::SeqCst), 0);
        assert!(mux.subscription_reconciliation_snapshot().is_none());

        let before_client1_attempts = client1.subscribe_attempts();
        let before_client2_attempts = client2.subscribe_attempts();

        let lru = AccountsLruCache::new(NonZeroUsize::new(10).unwrap());
        lru.add(pk);
        let secondary = AccountsLruCache::new(NonZeroUsize::new(10).unwrap());
        let (removed_tx, mut removed_rx) = mpsc::channel::<Pubkey>(10);

        let count = reconcile_subscriptions(
            &lru,
            &secondary,
            &mux,
            &[],
            &removed_tx,
            None,
            None,
            None,
            None,
            None,
        )
        .await;

        assert_eq!(count, 1);
        assert_eq!(client1.subscribe_attempts(), before_client1_attempts);
        assert_eq!(client2.subscribe_attempts(), before_client2_attempts);
        assert!(removed_rx.try_recv().is_err());

        mux.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn test_subscribe_skips_disconnected_client_during_reconnect() {
        init_logger();

        let (tx1, rx1) = mpsc::channel(10_000);
        let (tx2, rx2) = mpsc::channel(10_000);
        let client1 = Arc::new(ChainPubsubClientMock::new(tx1, rx1));
        let client2 = Arc::new(ChainPubsubClientMock::new(tx2, rx2));

        let pk = Pubkey::new_unique();
        let (mux, aborts) = new_submux_with_abort(
            vec![client1.clone(), client2.clone()],
            vec![pk],
            Some(100),
        );

        mux.subscribe(pk, None).await.unwrap();

        client1.disable_reconnect();
        client1.simulate_disconnect();
        aborts[0].send(()).await.expect("abort send");
        wait_for_connected_clients(&mux, 1).await;

        assert_eq!(mux.connected_clients.load(Ordering::SeqCst), 1);
        let client1_attempts = client1.subscribe_attempts();

        let pk2 = Pubkey::new_unique();
        mux.subscribe(pk2, None).await.unwrap();

        assert_eq!(client1.subscribe_attempts(), client1_attempts);
        assert!(!client1.subscriptions_union().contains(&pk2));
        assert!(client2.subscriptions_union().contains(&pk2));

        mux.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn test_connected_client_ids_lock_recovers_from_poison() {
        init_logger();

        let (tx, rx) = mpsc::channel(10_000);
        let client = Arc::new(ChainPubsubClientMock::new(tx, rx));
        let mux: SubMuxClient<ChainPubsubClientMock> =
            new_submux_client(vec![client], Some(100));

        let connected_client_ids = mux.connected_client_ids.clone();
        let _ = std::thread::spawn(move || {
            let _guard = connected_client_ids.lock().unwrap();
            panic!("poison connected_client_ids");
        })
        .join();

        assert_eq!(mux.connected_clients_snapshot().len(), 1);

        mux.shutdown().await.unwrap();
    }

    // -----------------
    // Dedup window expiry edge case
    // -----------------
    #[tokio::test]
    async fn test_dedup_same_slot_after_window_expires() {
        init_logger();

        let (tx1, rx1) = mpsc::channel(10_000);
        let (tx2, rx2) = mpsc::channel(10_000);
        let client1 = Arc::new(ChainPubsubClientMock::new(tx1, rx1));
        let client2 = Arc::new(ChainPubsubClientMock::new(tx2, rx2));

        // Use a short dedup window (100ms) so we can test expiry
        let mux: SubMuxClient<ChainPubsubClientMock> = new_submux_client(
            vec![client1.clone(), client2.clone()],
            Some(100),
        );
        let mut mux_rx = mux.take_updates();

        let pk = Pubkey::new_unique();
        mux.subscribe(pk, None).await.unwrap();

        // First delivery of (pk, slot=42) from client1
        client1
            .send_account_update(pk, 42, &account_with_lamports(100))
            .await;
        let first = tokio::time::timeout(
            std::time::Duration::from_millis(200),
            mux_rx.recv(),
        )
        .await
        .expect("first update expected")
        .expect("stream open");
        assert_eq!(first.pubkey, pk);
        assert_eq!(first.slot, 42);

        // Second delivery within the dedup window — should be deduped
        client2
            .send_account_update(pk, 42, &account_with_lamports(100))
            .await;
        let recv = tokio::time::timeout(
            std::time::Duration::from_millis(200),
            mux_rx.recv(),
        )
        .await;
        assert!(
            recv.is_err(),
            "same-slot update within dedup window should be suppressed"
        );

        // Wait for the dedup window to expire (100ms + margin)
        tokio::time::sleep(std::time::Duration::from_millis(120)).await;

        // Third delivery of the same (pk, slot=42) from client2
        // after the window expired — should be forwarded again
        client2
            .send_account_update(pk, 42, &account_with_lamports(100))
            .await;
        let after_expiry = tokio::time::timeout(
            std::time::Duration::from_millis(200),
            mux_rx.recv(),
        )
        .await
        .expect("update expected after dedup window expiry")
        .expect("stream open");
        assert_eq!(after_expiry.pubkey, pk);
        assert_eq!(after_expiry.slot, 42);

        mux.shutdown().await.unwrap();
    }
}
