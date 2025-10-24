use std::{
    cmp,
    collections::{HashMap, HashSet, VecDeque},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use async_trait::async_trait;
use log::*;
use solana_pubkey::Pubkey;
use tokio::sync::mpsc;

use crate::remote_account_provider::{
    chain_pubsub_client::ChainPubsubClient,
    errors::RemoteAccountProviderResult, pubsub_common::SubscriptionUpdate,
};

const SUBMUX_OUT_CHANNEL_SIZE: usize = 5_000;
const DEDUP_WINDOW_MILLIS: u64 = 2_000;
const DEBOUNCE_INTERVAL_MILLIS: u64 = 2_000;
const DEFAULT_RECYCLE_INTERVAL_MILLIS: u64 = 3_600_000;

mod debounce_state;
pub use self::debounce_state::DebounceState;

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

#[derive(Clone)]
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
pub struct SubMuxClient<T: ChainPubsubClient> {
    /// Underlying pubsub clients this mux controls and forwards to/from.
    clients: Vec<Arc<T>>,
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
}

/// Configuration for SubMuxClient
#[derive(Debug, Clone, Default)]
pub struct SubMuxClientConfig {
    /// The deduplication window in milliseconds.
    pub dedupe_window_millis: Option<u64>,
    /// The debounce interval in milliseconds.
    pub debounce_interval_millis: Option<u64>,
    /// The debounce detection window in milliseconds.
    pub debounce_detection_window_millis: Option<u64>,
    /// Interval (millis) at which to recycle inner client connections.
    /// If None, defaults to DEFAULT_RECYCLE_INTERVAL_MILLIS.
    pub recycle_interval_millis: Option<u64>,
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

impl<T: ChainPubsubClient> SubMuxClient<T> {
    pub fn new(
        clients: Vec<Arc<T>>,
        dedupe_window_millis: Option<u64>,
    ) -> Self {
        Self::new_with_debounce(
            clients,
            DebounceConfig {
                dedupe_window_millis,
                ..DebounceConfig::default()
            },
        )
    }

    pub fn new_with_debounce(
        clients: Vec<Arc<T>>,
        config: DebounceConfig,
    ) -> Self {
        Self::new_with_configs(clients, config, SubMuxClientConfig::default())
    }

    pub fn new_with_configs(
        clients: Vec<Arc<T>>,
        config: DebounceConfig,
        mux_config: SubMuxClientConfig,
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
            vec![solana_sdk::sysvar::clock::ID].into_iter().collect();

        let me = Self {
            clients,
            out_tx,
            out_rx: Arc::new(Mutex::new(Some(out_rx))),
            dedup_cache: dedup_cache.clone(),
            dedup_window,
            debounce_interval,
            debounce_detection_window,
            debounce_states: debounce_states.clone(),
            never_debounce,
        };

        // Spawn background tasks
        me.spawn_dedup_pruner();
        me.spawn_debounce_flusher();
        me.maybe_spawn_connection_recycler(mux_config.recycle_interval_millis);
        me
    }

    fn spawn_dedup_pruner(&self) {
        let window = self.dedup_window;
        let cache = self.dedup_cache.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(window).await;
                let now = Instant::now();
                let mut map = cache.lock().unwrap();
                map.retain(|_, ts| now.duration_since(*ts) <= window);
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
        tokio::spawn(async move {
            let tick = cmp::max(Duration::from_millis(10), interval / 4);
            loop {
                tokio::time::sleep(tick).await;
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
        });
    }

    fn maybe_spawn_connection_recycler(
        &self,
        recycle_interval_millis: Option<u64>,
    ) {
        // Disabled when the interval is explicitly Some(0)
        if recycle_interval_millis == Some(0) {
            return;
        }
        let recycle_clients = self.clients.clone();
        let interval = Duration::from_millis(
            recycle_interval_millis.unwrap_or(DEFAULT_RECYCLE_INTERVAL_MILLIS),
        );
        tokio::spawn(async move {
            let mut idx: usize = 0;
            loop {
                tokio::time::sleep(interval).await;
                if recycle_clients.is_empty() {
                    continue;
                }
                let len = recycle_clients.len();
                let i = idx % len;
                idx = (idx + 1) % len;
                let client = recycle_clients[i].clone();
                client.recycle_connections().await;
            }
        });
    }

    fn start_forwarders(&self) {
        let window = self.dedup_window;
        let debounce_interval = self.debounce_interval;
        let detection_window = self.debounce_detection_window;
        let allowed_count = self.allowed_in_debounce_window_count();

        for client in &self.clients {
            self.spawn_forwarder_for_client(
                client,
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
            if changed && log_enabled!(Level::Trace) {
                trace!(
                    "{} debounce for: {}. Millis between arrivals: {:?}",
                    debounce_state.label(),
                    pubkey,
                    debounce_state.arrival_deltas_ms()
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
}

#[async_trait]
impl<T: ChainPubsubClient> ChainPubsubClient for SubMuxClient<T> {
    async fn recycle_connections(&self) {
        // This recycles all inner clients which may not always make
        // sense. Thus we don't expect this call on the Multiplexer itself.
        for client in &self.clients {
            client.recycle_connections().await;
        }
    }

    async fn subscribe(
        &self,
        pubkey: Pubkey,
    ) -> RemoteAccountProviderResult<()> {
        for client in &self.clients {
            client.subscribe(pubkey).await?;
        }
        Ok(())
    }

    async fn unsubscribe(
        &self,
        pubkey: Pubkey,
    ) -> RemoteAccountProviderResult<()> {
        for client in &self.clients {
            client.unsubscribe(pubkey).await?;
        }
        Ok(())
    }

    async fn shutdown(&self) -> RemoteAccountProviderResult<()> {
        for client in &self.clients {
            client.shutdown().await?;
        }
        Ok(())
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
}

#[cfg(test)]
mod tests {
    use solana_account::Account;
    use tokio::sync::mpsc;

    use super::*;
    use crate::{
        remote_account_provider::chain_pubsub_client::mock::ChainPubsubClientMock,
        testing::{init_logger, utils::sleep_ms},
    };

    fn account_with_lamports(lamports: u64) -> Account {
        Account {
            lamports,
            ..Account::default()
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

        let mux: SubMuxClient<ChainPubsubClientMock> = SubMuxClient::new(
            vec![client1.clone(), client2.clone()],
            Some(100),
        );
        let mut mux_rx = mux.take_updates();

        let pk = Pubkey::new_unique();

        mux.subscribe(pk).await.unwrap();

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
    async fn test_submux_unsubscribe_stops_forwarding() {
        init_logger();

        let (tx1, rx1) = mpsc::channel(10_000);
        let (tx2, rx2) = mpsc::channel(10_000);
        let client1 = Arc::new(ChainPubsubClientMock::new(tx1, rx1));
        let client2 = Arc::new(ChainPubsubClientMock::new(tx2, rx2));

        let mux: SubMuxClient<ChainPubsubClientMock> = SubMuxClient::new(
            vec![client1.clone(), client2.clone()],
            Some(100),
        );
        let mut mux_rx = mux.take_updates();

        let pk = Pubkey::new_unique();

        mux.subscribe(pk).await.unwrap();

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

        let mux: SubMuxClient<ChainPubsubClientMock> = SubMuxClient::new(
            vec![client1.clone(), client2.clone()],
            Some(100),
        );
        let mut mux_rx = mux.take_updates();

        let pk = Pubkey::new_unique();
        mux.subscribe(pk).await.unwrap();

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

        let mux: SubMuxClient<ChainPubsubClientMock> = SubMuxClient::new(
            vec![client1.clone(), client2.clone()],
            Some(100),
        );
        let mut mux_rx = mux.take_updates();

        let pk = Pubkey::new_unique();
        mux.subscribe(pk).await.unwrap();

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

        let mux: SubMuxClient<ChainPubsubClientMock> = SubMuxClient::new(
            vec![client1.clone(), client2.clone(), client3.clone()],
            Some(100),
        );
        let mut mux_rx = mux.take_updates();

        let pk = Pubkey::new_unique();
        mux.subscribe(pk).await.unwrap();

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
            SubMuxClient::new_with_debounce(
                vec![client.clone()],
                DebounceConfig {
                    dedupe_window_millis: Some(100),
                    interval_millis: Some(200),
                    detection_window_millis: Some(1000),
                },
            );
        let mut mux_rx = mux.take_updates();
        let pk = Pubkey::new_unique();
        mux.subscribe(pk).await.unwrap();

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
            SubMuxClient::new_with_debounce(
                vec![client.clone()],
                DebounceConfig {
                    dedupe_window_millis: Some(100),
                    interval_millis: Some(200),
                    detection_window_millis: Some(1000),
                },
            );
        let mut mux_rx = mux.take_updates();
        let pk = Pubkey::new_unique();
        mux.subscribe(pk).await.unwrap();

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
            SubMuxClient::new_with_debounce(
                vec![client.clone()],
                DebounceConfig {
                    dedupe_window_millis: Some(100),
                    interval_millis: Some(200),
                    detection_window_millis: Some(1000),
                },
            );
        let mut mux_rx = mux.take_updates();
        let pk = Pubkey::new_unique();
        mux.subscribe(pk).await.unwrap();

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
            SubMuxClient::new_with_debounce(
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
            mux.subscribe(other).await.unwrap();
            let schedule: Vec<(u64, u64)> = (0..10).map(|i| (i, 50)).collect();
            send_schedule(client.clone(), other, 5000, &schedule).await;
            let received = drain_slots(&mut mux_rx, 800).await;
            assert!(received.len() < 10, "some updates should be debounced");
        }

        // 2. Now subscribe to sysvar::clock and send same rapid updates
        //    None should be debounced
        {
            let clock = solana_sdk::sysvar::clock::ID;
            mux.subscribe(clock).await.unwrap();

            let schedule: Vec<(u64, u64)> = (0..10).map(|i| (i, 50)).collect();
            send_schedule(client.clone(), clock, 5000, &schedule).await;

            let received = drain_slots(&mut mux_rx, 800).await;
            assert_eq!(received.len(), 10, "no updates should be debounced");
        }

        mux.shutdown().await.unwrap();
    }

    // -----------------
    // Connection recycling
    // -----------------
    async fn setup_recycling(
        interval_millis: Option<u64>,
    ) -> (
        SubMuxClient<ChainPubsubClientMock>,
        Arc<ChainPubsubClientMock>,
        Arc<ChainPubsubClientMock>,
        Arc<ChainPubsubClientMock>,
    ) {
        init_logger();
        let (tx1, rx1) = mpsc::channel(1);
        let (tx2, rx2) = mpsc::channel(1);
        let (tx3, rx3) = mpsc::channel(1);
        let c1 = Arc::new(ChainPubsubClientMock::new(tx1, rx1));
        let c2 = Arc::new(ChainPubsubClientMock::new(tx2, rx2));
        let c3 = Arc::new(ChainPubsubClientMock::new(tx3, rx3));

        let mux: SubMuxClient<ChainPubsubClientMock> =
            SubMuxClient::new_with_configs(
                vec![c1.clone(), c2.clone(), c3.clone()],
                DebounceConfig::default(),
                SubMuxClientConfig {
                    recycle_interval_millis: interval_millis,
                    ..SubMuxClientConfig::default()
                },
            );

        (mux, c1, c2, c3)
    }
    #[tokio::test]
    async fn test_connection_recycling_enabled() {
        let (mux, c1, c2, c3) = setup_recycling(Some(50)).await;

        // allow 4 intervals (at ~50ms each) -> calls: c1,c2,c3,c1
        tokio::time::sleep(Duration::from_millis(220)).await;

        assert_eq!(c1.recycle_calls(), 2);
        assert_eq!(c2.recycle_calls(), 1);
        assert_eq!(c3.recycle_calls(), 1);

        mux.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn test_connection_recycling_disabled() {
        let (mux, c1, c2, c3) = setup_recycling(Some(0)).await;

        // wait enough time to ensure it would have recycled if enabled
        tokio::time::sleep(Duration::from_millis(220)).await;

        assert_eq!(c1.recycle_calls(), 0);
        assert_eq!(c2.recycle_calls(), 0);
        assert_eq!(c3.recycle_calls(), 0);

        mux.shutdown().await.unwrap();
    }
}
