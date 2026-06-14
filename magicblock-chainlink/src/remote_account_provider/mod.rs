use std::{
    collections::{hash_map::Entry, HashMap},
    num::NonZeroUsize,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex, RwLock, Weak,
    },
    time::Duration,
};

pub(crate) use chain_pubsub_client::{
    ChainPubsubClient, ChainPubsubClientImpl, ReconnectableClient,
};
pub(crate) use chain_rpc_client::{ChainRpcClient, ChainRpcClientImpl};
use config::RemoteAccountProviderConfig;
pub(crate) use errors::{
    RemoteAccountProviderError, RemoteAccountProviderResult,
};
use futures_util::{
    future::{join_all, try_join_all},
    stream::{FuturesUnordered, StreamExt},
};
pub use lru_cache::{AccountsLruCache, AddAccountOutcome};
use magicblock_config::config::GrpcConfig;
pub(crate) use remote_account::RemoteAccount;
pub use remote_account::RemoteAccountUpdateSource;
use solana_account::Account;
use solana_account_decoder_client_types::UiAccountEncoding;
use solana_commitment_config::CommitmentConfig;
use solana_pubkey::Pubkey;
#[cfg(any(test, feature = "dev-context"))]
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_rpc_client_api::{
    client_error::ErrorKind, config::RpcAccountInfoConfig,
    custom_error::JSON_RPC_SERVER_ERROR_MIN_CONTEXT_SLOT_NOT_REACHED,
    request::RpcError,
};
use solana_sdk_ids::sysvar::clock;
use tokio::{
    sync::{mpsc, oneshot, Mutex as AsyncMutex},
    task, time,
};
use tracing::*;

pub mod chain_slot;
use chain_slot::ChainSlot;
pub(crate) mod chain_laser_actor;
pub mod chain_laser_client;
pub(crate) mod chain_pubsub_actor;
pub mod chain_pubsub_client;
pub mod chain_rpc_client;
pub mod chain_updates_client;
pub mod config;
pub mod endpoint;
pub mod errors;
mod lru_cache;
pub mod program_account;
pub mod pubsub_common;
pub mod pubsub_connection;
pub mod pubsub_connection_pool;
mod remote_account;
mod subscription_reconciler;

#[cfg(test)]
mod tests;

pub use endpoint::{Endpoint, Endpoints};
use magicblock_metrics::{
    metrics,
    metrics::{
        inc_account_fetches_failed, inc_account_fetches_found,
        inc_account_fetches_not_found, inc_account_fetches_success,
        set_monitored_accounts_count, AccountFetchOrigin,
    },
};
pub use remote_account::{ResolvedAccount, ResolvedAccountSharedData};

use crate::{
    errors::ChainlinkResult,
    remote_account_provider::{
        chain_updates_client::ChainUpdatesClient,
        pubsub_common::{SubscriptionSource, SubscriptionUpdate},
    },
    submux::SubMuxClient,
};

const ACTIVE_SUBSCRIPTIONS_UPDATE_INTERVAL_MS: u64 = 60_000;
pub(crate) const DEFAULT_SUBSCRIPTION_RETRIES: usize = 5;

type ChainUpdatesPubsub = (Arc<ChainUpdatesClient>, mpsc::Receiver<()>);

async fn connect_pubsub_client(
    ep: Endpoint,
    commitment: CommitmentConfig,
    rpc_client: ChainRpcClientImpl,
    chain_slot: Arc<AtomicU64>,
    resubscription_delay: Duration,
    grpc_cfg: GrpcConfig,
) -> (String, RemoteAccountProviderResult<ChainUpdatesPubsub>) {
    let ep_label = ep.label().to_string();
    let (abort_tx, abort_rx) = mpsc::channel(1);
    let client = ChainUpdatesClient::try_new_from_endpoint(
        &ep,
        commitment,
        abort_tx,
        chain_slot,
        resubscription_delay,
        rpc_client,
        &grpc_cfg,
    )
    .await;
    (ep_label, client.map(|c| (Arc::new(c), abort_rx)))
}

fn collect_connected_pubsubs(
    results: Vec<(String, RemoteAccountProviderResult<ChainUpdatesPubsub>)>,
) -> Vec<ChainUpdatesPubsub> {
    results
        .into_iter()
        .filter_map(|(label, result)| match result {
            Ok(client) => Some(client),
            Err(err) => {
                warn!(
                    endpoint = %label,
                    error = %err,
                    "Skipping pubsub client that failed to connect"
                );
                None
            }
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn spawn_deferred_pubsub_clients(
    endpoints: Vec<Endpoint>,
    commitment: CommitmentConfig,
    rpc_client: ChainRpcClientImpl,
    chain_slot: Arc<AtomicU64>,
    resubscription_delay: Duration,
    grpc_cfg: GrpcConfig,
    submux: SubMuxClient<ChainUpdatesClient>,
    subscribed_accounts: Arc<AccountsLruCache>,
    subscription_transition_lock: Arc<AsyncMutex<()>>,
) {
    tokio::spawn(async move {
        let mut pubsub_futs = endpoints
            .into_iter()
            .map(|ep| {
                connect_pubsub_client(
                    ep,
                    commitment,
                    rpc_client.clone(),
                    chain_slot.clone(),
                    resubscription_delay,
                    grpc_cfg.clone(),
                )
            })
            .collect::<FuturesUnordered<_>>();
        while let Some((label, result)) = pubsub_futs.next().await {
            let (client, abort_rx) = match result {
                Ok(client) => client,
                Err(err) => {
                    warn!(
                        endpoint = %label,
                        error = %err,
                        "Skipping deferred pubsub client that failed to connect"
                    );
                    continue;
                }
            };

            let add_result = {
                let _transition_guard =
                    subscription_transition_lock.lock().await;
                submux
                    .add_client(client, abort_rx, subscribed_accounts.clone())
                    .await
            };
            if let Err(err) = add_result {
                warn!(
                    endpoint = %label,
                    error = %err,
                    "Skipping deferred pubsub client that failed to attach"
                );
                continue;
            }
            debug!(endpoint = %label, "Attached deferred pubsub client");
        }
    });
}

// Maps pubkey -> (fetch_start_slot, requests_waiting)
type FetchResult = Result<RemoteAccount, RemoteAccountProviderError>;
type FetchingAccountGeneration = u64;

struct FetchingAccountState {
    generation: FetchingAccountGeneration,
    fetch_start_slot: u64,
    waiters: Vec<oneshot::Sender<FetchResult>>,
}

type FetchingAccounts = Mutex<HashMap<Pubkey, FetchingAccountState>>;

struct ClaimedSubscriptionSetupGuard {
    fetching_accounts: Arc<FetchingAccounts>,
    claimed_pubkeys: Vec<Pubkey>,
    claimed_generations: HashMap<Pubkey, FetchingAccountGeneration>,
    cancellation_error_text: Option<String>,
}

impl ClaimedSubscriptionSetupGuard {
    fn new(
        fetching_accounts: Arc<FetchingAccounts>,
        claimed_pubkeys: Vec<Pubkey>,
        claimed_generations: HashMap<Pubkey, FetchingAccountGeneration>,
    ) -> Self {
        Self {
            fetching_accounts,
            claimed_pubkeys,
            claimed_generations,
            cancellation_error_text: Some(
                "account subscription setup cancelled".to_string(),
            ),
        }
    }

    fn cleanup_with_error(&mut self, waiter_error_text: String) {
        {
            let mut fetching = self
                .fetching_accounts
                .lock()
                .unwrap_or_else(|err| err.into_inner());
            for pubkey in &self.claimed_pubkeys {
                let Some(generation) =
                    self.claimed_generations.get(pubkey).copied()
                else {
                    continue;
                };
                if let Some(state) =
                    remove_fetching_account_if_generation_matches(
                        &mut fetching,
                        pubkey,
                        generation,
                    )
                {
                    for sender in state.waiters {
                        let _ = sender.send(Err(
                            RemoteAccountProviderError::AccountSubscriptionsTaskFailed(
                                waiter_error_text.clone(),
                            ),
                        ));
                    }
                }
            }
        }
        self.disarm();
    }

    fn disarm(&mut self) {
        self.claimed_pubkeys.clear();
        self.claimed_generations.clear();
        self.cancellation_error_text = None;
    }
}

impl Drop for ClaimedSubscriptionSetupGuard {
    fn drop(&mut self) {
        let Some(waiter_error_text) = self.cancellation_error_text.take()
        else {
            return;
        };
        self.cleanup_with_error(waiter_error_text);
    }
}

/// Internal ownership/refcount key for shared pubsub subscriptions.
///
/// `DirectAccount` is normal remote-account monitoring and is the only
/// subscription reason that should participate in normal capacity eviction.
/// `UndelegationTracking` is protected ownership for delegated accounts that
/// are being undelegated and must never be treated as normal capacity-evictable
/// ownership.
///
/// Delegated accounts that are not undelegating are locally authoritative and
/// should have `DirectAccount` ownership released once delegation is discovered.
/// LRU membership is bookkeeping for live account subscriptions, but capacity
/// eviction may only remove entries that are not protected by account state or
/// ownership.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SubscriptionReason {
    DirectAccount,
    DelegationRecord,
    ProgramData,
    UndelegationTracking,
    AtaProjection,
}

#[derive(Debug, Default, Clone)]
struct SubscriptionOwnership {
    reasons: HashMap<SubscriptionReason, usize>,
}

impl SubscriptionOwnership {
    fn acquire(&mut self, reason: SubscriptionReason) {
        *self.reasons.entry(reason).or_default() += 1;
    }

    fn contains(&self, reason: SubscriptionReason) -> bool {
        self.reasons.contains_key(&reason)
    }

    fn release(&mut self, reason: SubscriptionReason) -> bool {
        match self.reasons.entry(reason) {
            Entry::Occupied(mut entry) => {
                let count = entry.get_mut();
                *count -= 1;
                if *count == 0 {
                    entry.remove();
                }
            }
            Entry::Vacant(_) => {}
        }
        self.reasons.is_empty()
    }

    fn release_all(&mut self, reason: SubscriptionReason) -> usize {
        self.reasons.remove(&reason).unwrap_or_default()
    }

    fn is_empty(&self) -> bool {
        self.reasons.is_empty()
    }
}

pub(crate) enum SubscriptionReleaseMode {
    Single,
    All,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CapacityEvictionProtection {
    pub delegated: bool,
    pub undelegating: bool,
    pub ephemeral: bool,
}

impl CapacityEvictionProtection {
    pub fn is_protected(self) -> bool {
        self.delegated || self.undelegating || self.ephemeral
    }
}

type CapacityEvictionProtectionPredicate =
    dyn Fn(&Pubkey) -> CapacityEvictionProtection + Send + Sync;
type SharedCapacityEvictionProtectionPredicate =
    Arc<RwLock<Option<Arc<CapacityEvictionProtectionPredicate>>>>;

pub struct ForwardedSubscriptionUpdate {
    pub pubkey: Pubkey,
    pub account: RemoteAccount,
    /// The upstream subscription stream that produced this update. Consumers
    /// must distinguish account-sub vs program-sub updates because a pubkey
    /// can be tracked solely via a program subscription (e.g. delegated
    /// accounts whose direct subscription was released after cloning).
    pub source: SubscriptionSource,
}

unsafe impl Send for ForwardedSubscriptionUpdate {}
unsafe impl Sync for ForwardedSubscriptionUpdate {}

// Not sure why helius uses a different code for this error
const HELIUS_CONTEXT_SLOT_NOT_REACHED: i64 = -32603;
const RPC_FETCH_MAX_RETRIES: u64 = 3;
const RPC_FETCH_RETRY_DELAY: Duration = Duration::from_millis(400);
const RPC_FETCH_TIMEOUT: Duration = Duration::from_secs(5);
const MATCH_SLOTS_MAX_TOTAL_TIME: Duration = Duration::from_secs(10);
pub struct RemoteAccountProvider<T: ChainRpcClient, U: ChainPubsubClient> {
    /// The RPC client to fetch accounts from chain the first time we receive
    /// a request for them
    rpc_client: T,
    /// The pubsub client to listen for updates on chain and keep the account
    /// states up to date
    pubsub_client: U,
    /// Minimal tracking of accounts currently being fetched to handle race conditions
    /// between fetch and subscription updates. Only used during active fetch operations.
    fetching_accounts: Arc<FetchingAccounts>,
    /// Monotonic generation for claimed fetching_accounts ownership.
    next_fetching_account_generation: AtomicU64,
    /// Subscription ownership reasons tracked per pubkey.
    subscription_ownership:
        Arc<AsyncMutex<HashMap<Pubkey, SubscriptionOwnership>>>,
    /// Serializes subscription transitions that can affect more than one
    /// pubkey. Acquiring one pubkey can evict and unsubscribe another pubkey
    /// from the LRU, so per-pubkey locks alone are not enough to keep
    /// ownership reasons and LRU membership in sync.
    subscription_transition_lock: Arc<AsyncMutex<()>>,
    /// Per-pubkey locks serializing subscription acquire/release transitions.
    ///
    /// Values are weak references so pubkeys do not accumulate forever after
    /// their transient transition lock is no longer in use.
    subscription_key_locks:
        Arc<AsyncMutex<HashMap<Pubkey, Weak<AsyncMutex<()>>>>>,
    /// The current slot on chain.
    ///
    /// This value is updated from two sources and always stores the maximum
    /// slot seen from either:
    ///
    /// 1. **WebSocket**: Updated in [RemoteAccountProvider::listen_for_account_updates] when clock
    ///    account (`clock::ID`) subscription updates are received.
    ///
    /// 2. **GRPC**: Updated directly in [chain_laser_actor::ChainLaserActor::process_subscription_update]
    ///    when slot updates [UpdateOneof::Slot] are received from the GRPC stream.
    ///
    /// Both sources use `fetch_max()` to ensure this value is monotonically
    /// increasing and reflects the highest known slot from any source.
    /// Metrics are automatically captured on updates inside [ChainSlot::update]
    chain_slot: ChainSlot,

    /// The slot of the last account update we received
    last_update_slot: Arc<AtomicU64>,

    /// The total number of account updates we received
    received_updates_count: Arc<AtomicU64>,

    /// Tracks which accounts are currently subscribed to
    lrucache_subscribed_accounts: Arc<AccountsLruCache>,

    capacity_eviction_protection: SharedCapacityEvictionProtectionPredicate,

    /// Channel to notify when an account is removed from the cache and thus no
    /// longer being watched
    removed_account_tx: mpsc::Sender<Pubkey>,
    /// Single listener channel sending an update when an account is removed
    /// and no longer being watched.
    removed_account_rx: Mutex<Option<mpsc::Receiver<Pubkey>>>,

    subscription_forwarder: Arc<mpsc::Sender<ForwardedSubscriptionUpdate>>,

    /// Task that periodically updates the active subscriptions gauge
    _active_subscriptions_task_handle: Option<task::JoinHandle<()>>,
}

// -----------------
// Configs
// -----------------
pub struct MatchSlotsConfig {
    pub max_retries: u64,
    pub retry_interval_ms: u64,
    pub min_context_slot: Option<u64>,
}

impl Default for MatchSlotsConfig {
    fn default() -> Self {
        Self {
            max_retries: 10,
            retry_interval_ms: 50,
            min_context_slot: None,
        }
    }
}

fn next_match_slots_retry(
    retries: &mut u64,
    start: std::time::Instant,
    config: &MatchSlotsConfig,
) -> Result<Duration, String> {
    *retries += 1;
    if *retries == config.max_retries {
        return Err(format!("max retries {}", config.max_retries));
    }
    if start.elapsed() > MATCH_SLOTS_MAX_TOTAL_TIME {
        return Err(format!(
            "max total time of {} seconds",
            MATCH_SLOTS_MAX_TOTAL_TIME.as_secs()
        ));
    }
    Ok(match_slots_retry_delay(config))
}

fn next_match_slots_rpc_error_retry(
    retries: &mut u64,
    start: std::time::Instant,
    config: &MatchSlotsConfig,
) -> Result<Duration, String> {
    next_match_slots_retry(retries, start, config)
        .map(|delay| delay.max(RPC_FETCH_RETRY_DELAY))
}

fn match_slots_retry_delay(config: &MatchSlotsConfig) -> Duration {
    Duration::from_millis(config.retry_interval_ms)
}

impl
    RemoteAccountProvider<ChainRpcClientImpl, SubMuxClient<ChainUpdatesClient>>
{
    pub async fn try_from_urls_and_config(
        endpoints: &Endpoints,
        commitment: CommitmentConfig,
        subscription_forwarder: mpsc::Sender<ForwardedSubscriptionUpdate>,
        config: &RemoteAccountProviderConfig,
        chain_slot: Option<Arc<AtomicU64>>,
    ) -> ChainlinkResult<
        Option<
            RemoteAccountProvider<
                ChainRpcClientImpl,
                SubMuxClient<ChainUpdatesClient>,
            >,
        >,
    > {
        let mode = config.lifecycle_mode();
        if mode.needs_remote_account_provider() {
            debug!("Creating RemoteAccountProvider");
            let provider = RemoteAccountProvider::<
                ChainRpcClientImpl,
                SubMuxClient<ChainUpdatesClient>,
            >::try_new_from_endpoints(
                endpoints,
                commitment,
                subscription_forwarder,
                config,
                chain_slot.unwrap_or_default(),
            )
            .await?;
            Ok(Some(provider))
        } else {
            Ok(None)
        }
    }
}

impl<T: ChainRpcClient, U: ChainPubsubClient> RemoteAccountProvider<T, U> {
    fn next_fetching_account_generation(&self) -> FetchingAccountGeneration {
        self.next_fetching_account_generation
            .fetch_add(1, Ordering::Relaxed)
            .wrapping_add(1)
    }

    pub async fn try_from_clients_and_mode(
        rpc_client: T,
        pubsub_client: U,
        subscription_forwarder: mpsc::Sender<ForwardedSubscriptionUpdate>,
        config: &RemoteAccountProviderConfig,
        lrucache_subscribed_accounts: Arc<AccountsLruCache>,
        chain_slot: Arc<AtomicU64>,
    ) -> ChainlinkResult<Option<RemoteAccountProvider<T, U>>> {
        let chain_slot = ChainSlot::new(chain_slot);
        if config.lifecycle_mode().needs_remote_account_provider() {
            Ok(Some(
                Self::new(
                    rpc_client,
                    pubsub_client,
                    subscription_forwarder,
                    config,
                    lrucache_subscribed_accounts,
                    chain_slot,
                )
                .await?,
            ))
        } else {
            Ok(None)
        }
    }

    /// Creates a background task that periodically updates the active subscriptions gauge
    fn start_active_subscriptions_updater<PubsubClient: ChainPubsubClient>(
        subscribed_accounts: Arc<AccountsLruCache>,
        pubsub_client: Arc<PubsubClient>,
        removed_account_tx: mpsc::Sender<Pubkey>,
    ) -> task::JoinHandle<()> {
        task::spawn(async move {
            let mut interval = time::interval(Duration::from_millis(
                ACTIVE_SUBSCRIPTIONS_UPDATE_INTERVAL_MS,
            ));
            let never_evicted = subscribed_accounts.never_evicted_accounts();

            loop {
                interval.tick().await;
                let pubsub_total =
                    subscription_reconciler::reconcile_subscriptions(
                        &subscribed_accounts,
                        pubsub_client.as_ref(),
                        &never_evicted,
                        &removed_account_tx,
                    )
                    .await;

                debug!(count = pubsub_total, "Updating active subscriptions");
                set_monitored_accounts_count(pubsub_total);
            }
        })
    }

    /// Creates a new instance of the remote account provider
    /// By the time this method returns the current chain slot was resolved and
    /// a subscription setup to keep it up to date.
    pub(crate) async fn new(
        rpc_client: T,
        pubsub_client: U,
        subscription_forwarder: mpsc::Sender<ForwardedSubscriptionUpdate>,
        config: &RemoteAccountProviderConfig,
        lrucache_subscribed_accounts: Arc<AccountsLruCache>,
        chain_slot: ChainSlot,
    ) -> RemoteAccountProviderResult<Self> {
        let (removed_account_tx, removed_account_rx) =
            tokio::sync::mpsc::channel(100);

        let active_subscriptions_updater =
            if config.enable_subscription_metrics() {
                Some(Self::start_active_subscriptions_updater(
                    lrucache_subscribed_accounts.clone(),
                    Arc::new(pubsub_client.clone()),
                    removed_account_tx.clone(),
                ))
            } else {
                None
            };

        let me = Self {
            fetching_accounts: Arc::<FetchingAccounts>::default(),
            next_fetching_account_generation: AtomicU64::default(),
            subscription_ownership: Arc::new(AsyncMutex::new(HashMap::new())),
            subscription_transition_lock: Arc::new(AsyncMutex::new(())),
            subscription_key_locks: Arc::new(AsyncMutex::new(HashMap::new())),
            rpc_client,
            pubsub_client,
            chain_slot,
            last_update_slot: Arc::<AtomicU64>::default(),
            received_updates_count: Arc::<AtomicU64>::default(),
            lrucache_subscribed_accounts,
            capacity_eviction_protection: Arc::new(RwLock::new(None)),
            subscription_forwarder: Arc::new(subscription_forwarder),
            removed_account_tx,
            removed_account_rx: Mutex::new(Some(removed_account_rx)),
            _active_subscriptions_task_handle: active_subscriptions_updater,
        };

        let updates = me.pubsub_client.take_updates();
        me.listen_for_account_updates(updates)?;
        let clock_remote_account = me
            .try_get(clock::ID, AccountFetchOrigin::GetAccount)
            .await?;
        match clock_remote_account {
            RemoteAccount::NotFound(_) => {
                Err(RemoteAccountProviderError::ClockAccountCouldNotBeResolved(
                    clock::ID.to_string(),
                ))
            }
            RemoteAccount::Found(_) => {
                me.chain_slot.update(clock_remote_account.slot());
                Ok(me)
            }
        }
    }

    pub async fn try_new_from_endpoints(
        endpoints: &Endpoints,
        commitment: CommitmentConfig,
        subscription_forwarder: mpsc::Sender<ForwardedSubscriptionUpdate>,
        config: &RemoteAccountProviderConfig,
        chain_slot: Arc<AtomicU64>,
    ) -> RemoteAccountProviderResult<
        RemoteAccountProvider<
            ChainRpcClientImpl,
            SubMuxClient<ChainUpdatesClient>,
        >,
    > {
        if endpoints.is_empty() {
            return Err(
                RemoteAccountProviderError::AccountSubscriptionsTaskFailed(
                    "No endpoints provided".to_string(),
                ),
            );
        }

        // Build RPC clients (use the first RPC endpoint found)
        let rpc_url = endpoints.rpc_url().ok_or_else(|| {
            RemoteAccountProviderError::AccountSubscriptionsTaskFailed(
                "No RPC endpoint found".to_string(),
            )
        })?;
        let rpc_client =
            ChainRpcClientImpl::new_from_url(rpc_url.as_str(), commitment);

        // Build startup pubsub clients and wrap them into a SubMuxClient.
        // gRPC clients are cheap to create and backfill subscriptions, so
        // when present we let slower WebSocket clients attach after startup.
        // If gRPC cannot subscribe during startup, retry once with WebSocket
        // endpoints so mixed configs still have a live fallback.
        let pubsubs = endpoints.pubsubs();
        let has_grpc =
            pubsubs.iter().any(|ep| matches!(ep, Endpoint::Grpc { .. }));
        let (startup_pubsubs, deferred_pubsubs): (Vec<_>, Vec<_>) = pubsubs
            .into_iter()
            .cloned()
            .partition(|ep| !has_grpc || matches!(ep, Endpoint::Grpc { .. }));
        let fallback_pubsubs = (has_grpc && !deferred_pubsubs.is_empty())
            .then(|| deferred_pubsubs.clone());

        match Self::try_new_from_pubsubs(
            startup_pubsubs,
            deferred_pubsubs,
            commitment,
            rpc_client.clone(),
            chain_slot.clone(),
            subscription_forwarder.clone(),
            config,
        )
        .await
        {
            Ok((provider, deferred_pubsubs)) => {
                if !deferred_pubsubs.is_empty() {
                    spawn_deferred_pubsub_clients(
                        deferred_pubsubs,
                        commitment,
                        rpc_client,
                        chain_slot,
                        config.resubscription_delay(),
                        config.grpc().clone(),
                        provider.pubsub_client.clone(),
                        provider.lrucache_subscribed_accounts.clone(),
                        provider.subscription_transition_lock.clone(),
                    );
                }
                Ok(provider)
            }
            Err(err) => {
                let Some(fallback_pubsubs) = fallback_pubsubs else {
                    return Err(err);
                };
                warn!(
                    error = %err,
                    "gRPC startup pubsub failed; retrying with WebSocket fallback"
                );
                let (provider, _) = Self::try_new_from_pubsubs(
                    fallback_pubsubs,
                    Vec::new(),
                    commitment,
                    rpc_client,
                    chain_slot,
                    subscription_forwarder,
                    config,
                )
                .await?;
                Ok(provider)
            }
        }
    }

    async fn try_new_from_pubsubs(
        startup_pubsubs: Vec<Endpoint>,
        mut deferred_pubsubs: Vec<Endpoint>,
        commitment: CommitmentConfig,
        rpc_client: ChainRpcClientImpl,
        chain_slot: Arc<AtomicU64>,
        subscription_forwarder: mpsc::Sender<ForwardedSubscriptionUpdate>,
        config: &RemoteAccountProviderConfig,
    ) -> RemoteAccountProviderResult<(
        RemoteAccountProvider<
            ChainRpcClientImpl,
            SubMuxClient<ChainUpdatesClient>,
        >,
        Vec<Endpoint>,
    )> {
        let resubscription_delay = config.resubscription_delay();
        let pubsub_futs = startup_pubsubs.into_iter().map(|ep| {
            connect_pubsub_client(
                ep,
                commitment,
                rpc_client.clone(),
                chain_slot.clone(),
                resubscription_delay,
                config.grpc().clone(),
            )
        });
        let results = join_all(pubsub_futs).await;
        let mut pubsubs = collect_connected_pubsubs(results);

        if pubsubs.is_empty() && !deferred_pubsubs.is_empty() {
            warn!(
                "No startup pubsub clients connected; falling back to deferred pubsub endpoints"
            );
            let fallback_pubsub_futs =
                deferred_pubsubs.iter().cloned().map(|ep| {
                    connect_pubsub_client(
                        ep,
                        commitment,
                        rpc_client.clone(),
                        chain_slot.clone(),
                        resubscription_delay,
                        config.grpc().clone(),
                    )
                });
            let results = join_all(fallback_pubsub_futs).await;
            pubsubs = collect_connected_pubsubs(results);
            deferred_pubsubs.clear();
        }

        if pubsubs.is_empty() {
            return Err(RemoteAccountProviderError::AllPubsubClientsFailed);
        }
        let subscribed_accounts = Arc::new(AccountsLruCache::new({
            // SAFETY: NonZeroUsize::new only returns None if the value is 0.
            // RemoteAccountProviderConfig can only be constructed with
            // capacity > 0
            let cap = config.subscribed_accounts_lru_capacity();
            NonZeroUsize::new(cap).expect("non-zero capacity")
        }));

        let submux =
            SubMuxClient::new(pubsubs, subscribed_accounts.clone(), None);

        if !config.program_subs().is_empty() {
            let count = config.program_subs().len();
            debug!(count, "Subscribing to program accounts");
            let subscribe_program_futs = config
                .program_subs()
                .iter()
                .map(|program_id| submux.subscribe_program(*program_id));
            try_join_all(subscribe_program_futs).await?;
        }

        let provider = RemoteAccountProvider::<
            ChainRpcClientImpl,
            SubMuxClient<ChainUpdatesClient>,
        >::new(
            rpc_client,
            submux,
            subscription_forwarder,
            config,
            subscribed_accounts,
            ChainSlot::new(chain_slot),
        )
        .await?;
        Ok((provider, deferred_pubsubs))
    }

    pub(crate) fn promote_accounts(&self, pubkeys: &[&Pubkey]) {
        self.lrucache_subscribed_accounts.promote_multi(pubkeys);
    }

    pub(crate) fn set_capacity_eviction_protection<F>(&self, predicate: F)
    where
        F: Fn(&Pubkey) -> CapacityEvictionProtection + Send + Sync + 'static,
    {
        let mut guard = self
            .capacity_eviction_protection
            .write()
            .unwrap_or_else(|poison| poison.into_inner());
        *guard = Some(Arc::new(predicate));
    }

    fn capacity_eviction_protection_for(
        &self,
        pubkey: &Pubkey,
    ) -> CapacityEvictionProtection {
        let guard = self
            .capacity_eviction_protection
            .read()
            .unwrap_or_else(|poison| poison.into_inner());
        guard.as_ref().map(|predicate| predicate(pubkey)).unwrap_or(
            CapacityEvictionProtection {
                delegated: false,
                undelegating: false,
                ephemeral: false,
            },
        )
    }

    pub fn try_get_removed_account_rx(
        &self,
    ) -> RemoteAccountProviderResult<mpsc::Receiver<Pubkey>> {
        let mut rx = self
            .removed_account_rx
            .lock()
            .expect("removed_account_rx lock poisoned");
        rx.take().ok_or_else(|| {
            RemoteAccountProviderError::LruCacheRemoveAccountSenderSupportsSingleReceiverOnly
        })
    }

    pub fn chain_slot(&self) -> u64 {
        self.chain_slot.load()
    }

    pub fn last_update_slot(&self) -> u64 {
        self.last_update_slot.load(Ordering::Relaxed)
    }

    pub fn received_updates_count(&self) -> u64 {
        self.received_updates_count.load(Ordering::Relaxed)
    }

    fn listen_for_account_updates(
        &self,
        mut updates: mpsc::Receiver<SubscriptionUpdate>,
    ) -> RemoteAccountProviderResult<()> {
        let fetching_accounts = self.fetching_accounts.clone();
        let chain_slot = self.chain_slot.clone();
        let received_updates_count = self.received_updates_count.clone();
        let last_update_slot = self.last_update_slot.clone();
        let subscription_forwarder = self.subscription_forwarder.clone();
        task::spawn(async move {
            while let Some(update) = updates.recv().await {
                let slot = update.slot;

                received_updates_count.fetch_add(1, Ordering::Relaxed);
                last_update_slot.store(slot, Ordering::Relaxed);

                if update.pubkey == clock::ID {
                    // We show as part of test_chain_pubsub_client_clock that the response
                    // context slot always matches the slot encoded in the slot data.
                    // Use fetch_max to ensure we always keep the highest slot value,
                    // since GRPC may have already updated chain_slot to a higher value.
                    chain_slot.update(slot);
                    // NOTE: we do not forward clock updates
                } else {
                    trace!(
                        pubkey = %update.pubkey,
                        slot,
                        "Received account update"
                    );
                    let remote_account = match update.account {
                        Some(account) => RemoteAccount::from_fresh_account(
                            account,
                            slot,
                            RemoteAccountUpdateSource::Subscription,
                        ),
                        None => {
                            warn!(
                                pubkey = %update.pubkey,
                                "Account update could not be decoded"
                            );
                            RemoteAccount::NotFound(slot)
                        }
                    };

                    // Check if we're currently fetching this account
                    let forward_update = {
                        let mut fetching = fetching_accounts
                            .lock()
                            .expect("fetching_accounts lock poisoned");
                        if let Some(generation) = fetching
                            .get(&update.pubkey)
                            .map(|state| state.generation)
                        {
                            if let Some(state) =
                                remove_fetching_account_if_generation_matches(
                                    &mut fetching,
                                    &update.pubkey,
                                    generation,
                                )
                            {
                                // If subscription update is newer than when we started fetching,
                                // resolve with the subscription data instead
                                if slot >= state.fetch_start_slot {
                                    trace!(pubkey = %update.pubkey, slot = slot, fetch_start_slot = state.fetch_start_slot, generation, "Using subscription update instead of fetch");

                                    // Resolve all pending requests with subscription data
                                    for sender in state.waiters {
                                        let _ = sender
                                            .send(Ok(remote_account.clone()));
                                    }
                                    None
                                } else {
                                    // Subscription is stale, put the fetch tracking back
                                    debug!(pubkey = %update.pubkey, slot = slot, fetch_start_slot = state.fetch_start_slot, generation, "Received stale subscription update");
                                    fetching.insert(update.pubkey, state);
                                    None
                                }
                            } else {
                                None
                            }
                        } else {
                            Some(ForwardedSubscriptionUpdate {
                                pubkey: update.pubkey,
                                account: remote_account,
                                source: update.source,
                            })
                        }
                    };

                    if let Some(forward_update) = forward_update {
                        if let Err(err) =
                            subscription_forwarder.send(forward_update).await
                        {
                            warn!(
                                pubkey = %update.pubkey,
                                error = ?err,
                                "Failed to forward subscription update"
                            );
                        }
                    }
                }
            }
        });
        Ok(())
    }

    /// Convenience wrapper around [`RemoteAccountProvider::try_get_multi`] to fetch
    /// a single account.
    #[instrument(skip(self))]
    pub async fn try_get(
        &self,
        pubkey: Pubkey,
        fetch_origin: AccountFetchOrigin,
    ) -> RemoteAccountProviderResult<RemoteAccount> {
        self.try_get_multi(&[pubkey], None, fetch_origin, None)
            .await
            // SAFETY: we are guaranteed to have a single result here as
            // otherwise we would have gotten an error
            .map(|mut accs| accs.drain(..).next().unwrap())
    }

    #[instrument(skip(self, pubkeys, config))]
    pub async fn try_get_multi_until_slots_match(
        &self,
        pubkeys: &[Pubkey],
        config: Option<MatchSlotsConfig>,
        fetch_origin: AccountFetchOrigin,
    ) -> RemoteAccountProviderResult<Vec<RemoteAccount>> {
        use SlotsMatchResult::*;

        // 1. Fetch the _normal_ way and hope the slots match and if required
        //    the min_context_slot is met
        let mut remote_accounts = self
            .try_get_multi(pubkeys, None, fetch_origin, None)
            .await?;
        if let Match = slots_match_and_meet_min_context(
            &remote_accounts,
            config.as_ref().and_then(|c| c.min_context_slot),
        ) {
            return Ok(remote_accounts);
        }

        let config = config.unwrap_or_default();
        // Capture the fetch start slot once and reuse it across retries. When a
        // caller provides a stricter min_context_slot, the forced refetch must
        // start from that slot rather than the provider's possibly lagging
        // chain slot.
        let fetch_start_slot = self
            .chain_slot
            .load()
            .max(config.min_context_slot.unwrap_or_default());
        // 2. Wait for the slots to match. Once the fast path mixed slots,
        // retry with an RPC-only batch so all accounts share one response slot.
        let start = std::time::Instant::now();
        let mut retries = 0;
        loop {
            if tracing::enabled!(tracing::Level::TRACE) {
                let slots = account_slots(&remote_accounts);
                let pubkey_slots = pubkeys
                    .iter()
                    .zip(slots)
                    .map(|(pk, slot)| format!("{pk}:{slot}"))
                    .collect::<Vec<_>>()
                    .join(", ");
                trace!(
                    "Retry({}) account fetch to sync non-matching slots [{}]",
                    retries + 1,
                    pubkey_slots
                );
            }
            remote_accounts = match self
                .fetch_multi_rpc_only(pubkeys, fetch_start_slot, fetch_origin)
                .await
            {
                Ok(remote_accounts) => remote_accounts,
                Err(err) => {
                    let retry = next_match_slots_rpc_error_retry(
                        &mut retries,
                        start,
                        &config,
                    );
                    debug!(
                        pubkeys = %pubkeys_str(pubkeys),
                        min_context_slot = ?config.min_context_slot,
                        retries = retries,
                        elapsed_ms = start.elapsed().as_millis() as u64,
                        error = %err,
                        "RPC-only account fetch failed while resolving accounts to compatible slots"
                    );
                    match retry {
                        Ok(retry_delay) => {
                            tokio::time::sleep(retry_delay).await;
                            continue;
                        }
                        Err(_) => return Err(err),
                    }
                }
            };
            let slots_match_result = slots_match_and_meet_min_context(
                &remote_accounts,
                config.min_context_slot,
            );
            if let Match = slots_match_result {
                return Ok(remote_accounts);
            }

            match next_match_slots_retry(&mut retries, start, &config) {
                Ok(retry_delay) => {
                    // If the slots don't match then wait for a bit and retry
                    tokio::time::sleep(retry_delay).await;
                    continue;
                }
                Err(limit) => {
                    let remote_account_slots = account_slots(&remote_accounts);
                    let remote_account_sources = remote_accounts
                        .iter()
                        .map(|account| account.source())
                        .collect::<Vec<_>>();
                    warn!(
                        pubkeys = %pubkeys_str(pubkeys),
                        slots = ?remote_account_slots,
                        sources = ?remote_account_sources,
                        min_context_slot = ?config.min_context_slot,
                        retries = retries,
                        elapsed_ms = start.elapsed().as_millis() as u64,
                        limit = %limit,
                        "Failed to resolve accounts to compatible slots"
                    );
                    match slots_match_result {
                        // SAFETY: Match case is already handled and returns
                        Match => unreachable!("we would have returned above"),
                        Mismatch => {
                            return Err(
                                RemoteAccountProviderError::SlotsDidNotMatch(
                                    pubkeys_str(pubkeys),
                                    remote_account_slots,
                                    limit,
                                ),
                            );
                        }
                        MatchButBelowMinContextSlot(slot) => {
                            return Err(RemoteAccountProviderError::MatchingSlotsNotSatisfyingMinContextSlot(
                                pubkeys_str(pubkeys),
                                remote_account_slots,
                                slot,
                                limit,
                            ));
                        }
                    }
                }
            }
        }
    }

    /// Gets the accounts for the given pubkeys by fetching from RPC.
    /// Always fetches fresh data. FetchCloner handles request deduplication.
    /// Subscribes first to catch any updates that arrive during fetch.
    #[instrument(skip(self, pubkeys, mark_empty_if_not_found))]
    pub async fn try_get_multi(
        &self,
        pubkeys: &[Pubkey],
        mark_empty_if_not_found: Option<&[Pubkey]>,
        fetch_origin: AccountFetchOrigin,
        fetch_start_slot: Option<u64>,
    ) -> RemoteAccountProviderResult<Vec<RemoteAccount>> {
        if pubkeys.is_empty() {
            return Ok(vec![]);
        }

        if tracing::enabled!(tracing::Level::TRACE) {
            trace!("Fetching accounts");
        }

        let fetch_start_slot =
            fetch_start_slot.unwrap_or_else(|| self.chain_slot.load());

        // Receivers awaited by this call. One entry per input pubkey, in
        // input order. Each receiver corresponds to a sender that was
        // pushed into the FetchingAccountState.waiters queue (either
        // because this call inserted the entry, or because it joined an
        // existing in-flight fetch as a waiter).
        let mut await_receivers: Vec<(Pubkey, oneshot::Receiver<FetchResult>)> =
            Vec::with_capacity(pubkeys.len());

        // Pubkeys this call actually inserted.
        // Only these pubkeys cause side effects (subscription setup, fetch)
        // by this call. Waiter-only pubkeys already have a subscription owned
        // by the original claimer and are being fetched by them.
        let mut claimed_pubkeys: Vec<Pubkey> = Vec::new();
        let mut claimed_generations: HashMap<
            Pubkey,
            FetchingAccountGeneration,
        > = HashMap::new();

        {
            let mut fetching = self.fetching_accounts.lock().unwrap();
            for &pubkey in pubkeys {
                let (sender, receiver) = oneshot::channel();
                let mut claimed = false;
                match fetching.entry(pubkey) {
                    Entry::Occupied(mut entry) => {
                        entry.get_mut().waiters.push(sender);
                    }
                    Entry::Vacant(entry) => {
                        let generation =
                            self.next_fetching_account_generation();
                        entry.insert(FetchingAccountState {
                            generation,
                            fetch_start_slot,
                            waiters: vec![sender],
                        });
                        claimed_generations.insert(pubkey, generation);
                        claimed = true;
                    }
                }
                if claimed {
                    claimed_pubkeys.push(pubkey);
                }
                await_receivers.push((pubkey, receiver));
            }
        }

        // Setup subscriptions and trigger the fetch only for pubkeys this
        // call actually claimed. Waiter-only pubkeys already have a
        // subscription and an in-flight fetch owned by the original
        // claimer; doing it again here would duplicate work and (for
        // setup_subscriptions) double-count subscription side effects.
        if !claimed_pubkeys.is_empty() {
            let mut subscription_setup_guard =
                ClaimedSubscriptionSetupGuard::new(
                    self.fetching_accounts.clone(),
                    claimed_pubkeys.clone(),
                    claimed_generations.clone(),
                );
            if let Err(err) = self.setup_subscriptions(&claimed_pubkeys).await {
                subscription_setup_guard.cleanup_with_error(err.to_string());
                return Err(err);
            }
            subscription_setup_guard.disarm();

            // Start the fetch for the claimed pubkeys only.
            let min_context_slot = fetch_start_slot;
            self.fetch(
                claimed_pubkeys.clone(),
                claimed_generations,
                mark_empty_if_not_found,
                min_context_slot,
                fetch_origin,
            );
        }

        // Wait for all accounts to resolve (either from fetch or
        // subscription override). We await receivers in input pubkey
        // order so the returned Vec is index-aligned with `pubkeys`.
        let mut resolved_accounts = vec![];
        let mut errors = vec![];

        for (idx, (pubkey, receiver)) in await_receivers.into_iter().enumerate()
        {
            match receiver.await {
                Ok(result) => match result {
                    Ok(remote_account) => {
                        resolved_accounts.push(remote_account)
                    }
                    Err(err) => {
                        warn!(pubkey = %pubkey, error = %err, "Failed to fetch account");
                        errors.push((idx, err));
                    }
                },
                Err(err) => {
                    warn!(pubkey = %pubkey, stream_index = idx, error = ?err, total_pubkeys = pubkeys.len(), "Failed to resolve account (unexpected RecvError)");
                    errors.push((
                        idx,
                        RemoteAccountProviderError::RecvrError(err),
                    ));
                }
            }
        }

        if errors.is_empty() {
            assert_eq!(
                resolved_accounts.len(),
                pubkeys.len(),
                "BUG: resolved accounts and pubkeys length mismatch"
            );
            Ok(resolved_accounts)
        } else {
            Err(RemoteAccountProviderError::AccountResolutionsFailed(
                errors
                    .iter()
                    .map(|(idx, err)| {
                        let pubkey = pubkeys
                            .get(*idx)
                            .map(|pk| pk.to_string())
                            .unwrap_or_else(|| {
                                "BUG: could not match pubkey".to_string()
                            });
                        format!("{pubkey}: {err:?}")
                    })
                    .collect::<Vec<_>>()
                    .join(",\n"),
            ))
        }
    }

    async fn fetch_multi_rpc_only(
        &self,
        pubkeys: &[Pubkey],
        min_context_slot: u64,
        fetch_origin: AccountFetchOrigin,
    ) -> RemoteAccountProviderResult<Vec<RemoteAccount>> {
        let config = RpcAccountInfoConfig {
            commitment: Some(self.rpc_client.commitment()),
            min_context_slot: Some(min_context_slot),
            encoding: Some(UiAccountEncoding::Base64Zstd),
            data_slice: None,
        };

        metrics::inc_remote_account_provider_a_count();
        let response = tokio::time::timeout(RPC_FETCH_TIMEOUT, async {
            self.rpc_client
                .get_multiple_accounts_with_config(pubkeys, config)
                .await
        })
        .await
        .map_err(|_| {
            RemoteAccountProviderError::AccountResolutionsFailed(format!(
                "RPC call timeout fetching accounts {} after {}ms",
                pubkeys_str(pubkeys),
                RPC_FETCH_TIMEOUT.as_millis()
            ))
        })?
        .map_err(|err| {
            RemoteAccountProviderError::AccountResolutionsFailed(format!(
                "RpcError fetching accounts {}: {err:?}",
                pubkeys_str(pubkeys)
            ))
        })?;

        let response_slot = response.context.slot;
        if response_slot < min_context_slot {
            return Err(RemoteAccountProviderError::AccountResolutionsFailed(
                format!(
                    "Response slot {response_slot} < {min_context_slot} fetching accounts {}",
                    pubkeys_str(pubkeys)
                ),
            ));
        }

        let response_value_len = response.value.len();
        if response_value_len != pubkeys.len() {
            return Err(RemoteAccountProviderError::AccountResolutionsFailed(
                format!(
                    "RPC returned {response_value_len} account results for {} requested accounts: {}",
                    pubkeys.len(),
                    pubkeys_str(pubkeys)
                ),
            ));
        }

        let mut found_count = 0u64;
        let mut not_found_count = 0u64;
        let remote_accounts = response
            .value
            .into_iter()
            .map(|account| match account {
                Some(account) => {
                    found_count += 1;
                    RemoteAccount::from_fresh_account(
                        account,
                        response_slot,
                        RemoteAccountUpdateSource::Fetch,
                    )
                }
                None => {
                    not_found_count += 1;
                    RemoteAccount::NotFound(response_slot)
                }
            })
            .collect();

        inc_account_fetches_success(pubkeys.len() as u64);
        inc_account_fetches_found(fetch_origin, found_count);
        inc_account_fetches_not_found(fetch_origin, not_found_count);

        Ok(remote_accounts)
    }

    async fn setup_subscriptions(
        &self,
        pubkeys: &[Pubkey],
    ) -> RemoteAccountProviderResult<()> {
        if tracing::enabled!(tracing::Level::TRACE) {
            let pubkeys_str = pubkeys
                .iter()
                .map(|pk| pk.to_string())
                .collect::<Vec<_>>()
                .join(", ");
            trace!(pubkeys = pubkeys_str, "Subscribing to accounts");
        }
        // Send all subscription requests in parallel (non-fail-fast).
        // We use join_all instead of try_join_all to ensure ALL acquire
        // attempts complete, even if some fail.
        let subscription_results =
            join_all(pubkeys.iter().map(|pubkey| async {
                self.acquire_subscription(
                    pubkey,
                    SubscriptionReason::DirectAccount,
                )
                .await
            }))
            .await;

        let mut errors = Vec::new();
        let mut acquired = Vec::new();
        for (result, pubkey) in subscription_results.iter().zip(pubkeys.iter())
        {
            match result {
                Err(err) => {
                    error!(
                        pubkey = %pubkey, err = ?err,
                        "Failed to subscribe to account"
                    );
                    errors.push(format!("{}: {}", pubkey, err));
                }
                Ok(()) => acquired.push(*pubkey),
            }
        }

        if !errors.is_empty() {
            for pubkey in &acquired {
                if let Err(unsub_err) = self
                    .release_single_subscription(
                        pubkey,
                        SubscriptionReason::DirectAccount,
                    )
                    .await
                {
                    if matches!(
                        unsub_err,
                        RemoteAccountProviderError::AccountSubscriptionDoesNotExist(_)
                    ) {
                        debug!(
                            pubkey = %pubkey, err = ?unsub_err,
                            "Failed to unsubscribe after partial \
                             subscription failure"
                        );
                    } else {
                        warn!(
                            pubkey = %pubkey, err = ?unsub_err,
                            "Failed to unsubscribe after partial \
                             subscription failure"
                        );
                    }
                }
            }
            return Err(
                RemoteAccountProviderError::AccountSubscriptionsTaskFailed(
                    format!(
                        "{} subscription(s) failed: [{}]",
                        errors.len(),
                        errors.join(", ")
                    ),
                ),
            );
        }

        Ok(())
    }

    /// Registers a new subscription for the given pubkey.
    async fn register_subscription(
        &self,
        pubkey: &Pubkey,
        reason: SubscriptionReason,
    ) -> RemoteAccountProviderResult<()> {
        // 1. First realize subscription
        if self.capacity_eviction_protection_for(pubkey).ephemeral {
            trace!(pubkey = %pubkey, "Skipping subscription for ephemeral account");
            return Ok(());
        }
        self.pubsub_client.subscribe(*pubkey, None).await?;

        // 2. Add to LRU cache
        // If an account is evicted then we need to unsubscribe from it
        // and then inform upstream that we are no longer tracking it
        let add_outcome = {
            let ownership = self.subscription_ownership.lock().await;
            self.lrucache_subscribed_accounts.add_with_evict_filter(
                *pubkey,
                |candidate| {
                    if !self.lrucache_subscribed_accounts.can_evict(candidate) {
                        trace!(
                            candidate = %candidate,
                            "Skipping capacity eviction candidate from never-evict set"
                        );
                        return false;
                    }

                    let protection =
                        self.capacity_eviction_protection_for(candidate);
                    if protection.is_protected() {
                        trace!(
                            candidate = %candidate,
                            delegated = protection.delegated,
                            undelegating = protection.undelegating,
                            "Skipping capacity eviction candidate protected by bank state"
                        );
                        return false;
                    }

                    let protected_by_ownership =
                        ownership.get(candidate).is_some_and(|ownership| {
                            ownership.contains(
                                SubscriptionReason::UndelegationTracking,
                            )
                        });
                    if protected_by_ownership {
                        trace!(
                            candidate = %candidate,
                            reason = ?SubscriptionReason::UndelegationTracking,
                            "Skipping capacity eviction candidate protected by subscription ownership"
                        );
                    }
                    !protected_by_ownership
                },
            )
        };

        match add_outcome {
            AddAccountOutcome::AlreadyPresent | AddAccountOutcome::Added => {}
            AddAccountOutcome::Evicted(evicted) => {
                trace!(evicted = %evicted, "Evicting account");

                // LRU eviction is a forced full removal, but keep local
                // ownership intact until pubsub confirms the subscription is
                // gone so a failed unsubscribe cannot leave ownership state
                // inconsistent with the active subscription.
                if let Err(err) = self.pubsub_client.unsubscribe(evicted).await
                {
                    if matches!(
                        err,
                        RemoteAccountProviderError::AccountSubscriptionDoesNotExist(
                            _
                        )
                    ) {
                        debug!(evicted = %evicted, error = ?err, "Failed to unsubscribe from pubsub for evicted account");
                    } else {
                        // Should we retry here?
                        warn!(evicted = %evicted, error = ?err, "Failed to unsubscribe from pubsub for evicted account");
                    }
                    self.subscription_ownership
                        .lock()
                        .await
                        .entry(*pubkey)
                        .or_default()
                        .acquire(reason);
                    return Err(err);
                }

                self.subscription_ownership.lock().await.remove(&evicted);

                // Inform upstream so it can remove it from the store. Failure
                // to notify is non-fatal here because the LRU, pubsub, and
                // ownership state have already been updated consistently.
                if let Err(err) = self.send_removal_update(evicted).await {
                    warn!(evicted = %evicted, error = ?err, "Failed to send removal update for evicted account");
                }
            }
            AddAccountOutcome::NoEvictableCandidate => {
                if let Err(err) = self.pubsub_client.unsubscribe(*pubkey).await
                {
                    debug!(
                        pubkey = %pubkey,
                        error = ?err,
                        "Failed to unsubscribe new subscription after all LRU candidates were protected"
                    );
                    return Err(err);
                }
                self.subscription_ownership.lock().await.remove(pubkey);
                self.lrucache_subscribed_accounts.remove(pubkey);
                debug!(
                    pubkey = %pubkey,
                    "No evictable subscription capacity available; all LRU candidates are protected"
                );
                return Err(
                    RemoteAccountProviderError::NoEvictableSubscriptionCapacity {
                        pubkey: *pubkey,
                    },
                );
            }
        }

        Ok(())
    }

    pub(crate) async fn send_removal_update(
        &self,
        evicted: Pubkey,
    ) -> RemoteAccountProviderResult<()> {
        self.removed_account_tx.send(evicted).await.map_err(
            RemoteAccountProviderError::FailedToSendAccountRemovalUpdate,
        )?;
        Ok(())
    }

    /// Check if an account is currently being watched (subscribed to)
    /// This does not consider accounts like the clock sysvar that are watched as
    /// part of the provider's internal logic.
    pub fn is_watching(&self, pubkey: &Pubkey) -> bool {
        self.lrucache_subscribed_accounts.contains(pubkey)
    }

    pub(crate) async fn evict_unwatched_with_subscription_lock<F, Fut>(
        &self,
        pubkey: &Pubkey,
        evict: F,
    ) -> bool
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = ()>,
    {
        let subscription_key_lock = self.subscription_key_lock(pubkey).await;
        let _subscription_guard = subscription_key_lock.lock().await;

        if self.is_watching(pubkey) {
            return false;
        }

        evict().await;
        true
    }

    /// Check if an account is currently pending (being fetched)
    pub fn is_pending(&self, pubkey: &Pubkey) -> bool {
        let fetching = self.fetching_accounts.lock().unwrap();
        fetching.contains_key(pubkey)
    }

    async fn subscription_key_lock(
        &self,
        pubkey: &Pubkey,
    ) -> Arc<AsyncMutex<()>> {
        let mut locks = self.subscription_key_locks.lock().await;
        locks.retain(|_, lock| lock.strong_count() > 0);

        if let Some(lock) = locks.get(pubkey).and_then(Weak::upgrade) {
            return lock;
        }

        let lock = Arc::new(AsyncMutex::new(()));
        locks.insert(*pubkey, Arc::downgrade(&lock));
        lock
    }

    pub async fn acquire_subscription(
        &self,
        pubkey: &Pubkey,
        reason: SubscriptionReason,
    ) -> RemoteAccountProviderResult<()> {
        self.acquire_subscription_with_mode(pubkey, reason, false)
            .await
    }

    pub async fn ensure_subscription(
        &self,
        pubkey: &Pubkey,
        reason: SubscriptionReason,
    ) -> RemoteAccountProviderResult<()> {
        self.acquire_subscription_with_mode(pubkey, reason, true)
            .await
    }

    #[cfg(test)]
    pub(crate) async fn has_subscription_reason(
        &self,
        pubkey: &Pubkey,
        reason: SubscriptionReason,
    ) -> bool {
        self.subscription_ownership
            .lock()
            .await
            .get(pubkey)
            .is_some_and(|ownership| ownership.contains(reason))
    }

    async fn acquire_subscription_with_mode(
        &self,
        pubkey: &Pubkey,
        reason: SubscriptionReason,
        skip_existing_reason: bool,
    ) -> RemoteAccountProviderResult<()> {
        let _transition_guard = self.subscription_transition_lock.lock().await;
        let subscription_key_lock = self.subscription_key_lock(pubkey).await;
        let _subscription_guard = subscription_key_lock.lock().await;

        let mut ownership = self.subscription_ownership.lock().await;
        if let Some(existing) = ownership.get_mut(pubkey) {
            if !skip_existing_reason || !existing.contains(reason) {
                existing.acquire(reason);
            }
            self.lrucache_subscribed_accounts.add(*pubkey);
            return Ok(());
        }
        drop(ownership);

        self.register_subscription(pubkey, reason).await?;

        let mut ownership = self.subscription_ownership.lock().await;
        ownership.entry(*pubkey).or_default().acquire(reason);
        Ok(())
    }

    pub async fn release_single_subscription(
        &self,
        pubkey: &Pubkey,
        reason: SubscriptionReason,
    ) -> RemoteAccountProviderResult<bool> {
        self.release_subscription_with_mode(
            pubkey,
            reason,
            SubscriptionReleaseMode::Single,
        )
        .await
    }

    pub(crate) async fn release_subscription_with_mode(
        &self,
        pubkey: &Pubkey,
        reason: SubscriptionReason,
        mode: SubscriptionReleaseMode,
    ) -> RemoteAccountProviderResult<bool> {
        let _transition_guard = self.subscription_transition_lock.lock().await;
        let subscription_key_lock = self.subscription_key_lock(pubkey).await;
        let _subscription_guard = subscription_key_lock.lock().await;

        if !self.lrucache_subscribed_accounts.can_evict(pubkey) {
            return Ok(false);
        }

        let released_count = {
            let mut ownership = self.subscription_ownership.lock().await;
            let (is_empty, released_count) = match ownership.get_mut(pubkey) {
                Some(existing) => {
                    let released_count = match mode {
                        SubscriptionReleaseMode::Single => {
                            existing.release(reason);
                            1
                        }
                        SubscriptionReleaseMode::All => {
                            existing.release_all(reason)
                        }
                    };
                    (existing.is_empty(), released_count)
                }
                None => return Ok(false),
            };
            if !is_empty {
                return Ok(false);
            }
            ownership.remove(pubkey);
            released_count
        };

        let success = subscription_reconciler::unsubscribe_and_notify_removal(
            *pubkey,
            &self.pubsub_client,
            &self.removed_account_tx,
        )
        .await;

        if success {
            self.lrucache_subscribed_accounts.remove(pubkey);
        } else {
            let mut ownership = self.subscription_ownership.lock().await;
            if ownership
                .get(pubkey)
                .is_none_or(SubscriptionOwnership::is_empty)
            {
                let ownership = ownership.entry(*pubkey).or_default();
                for _ in 0..released_count {
                    ownership.acquire(reason);
                }
            }
        }

        Ok(success)
    }

    pub(crate) async fn release_subscription_reason_silently_for_delegated_account(
        &self,
        pubkey: &Pubkey,
        reason: SubscriptionReason,
    ) -> RemoteAccountProviderResult<bool> {
        let _transition_guard = self.subscription_transition_lock.lock().await;
        let subscription_key_lock = self.subscription_key_lock(pubkey).await;
        let _subscription_guard = subscription_key_lock.lock().await;

        let released_count = {
            let release_mode = SubscriptionReleaseMode::All;
            let mut ownership = self.subscription_ownership.lock().await;
            let (is_empty, released_count) = match ownership.get_mut(pubkey) {
                Some(existing) => {
                    let released_count = match release_mode {
                        SubscriptionReleaseMode::Single => {
                            existing.release(reason);
                            1
                        }
                        SubscriptionReleaseMode::All => {
                            existing.release_all(reason)
                        }
                    };
                    (existing.is_empty(), released_count)
                }
                None => return Ok(false),
            };

            if released_count == 0 {
                return Ok(false);
            }

            if !is_empty {
                trace!(
                    pubkey = %pubkey,
                    ?reason,
                    released_count,
                    "Released delegated-account subscription ownership; \
                     kept protected/live subscription and LRU entry"
                );
                return Ok(false);
            }

            ownership.remove(pubkey);
            released_count
        };

        match self.pubsub_client.unsubscribe(*pubkey).await {
            Ok(()) => {
                self.lrucache_subscribed_accounts.remove(pubkey);
                trace!(
                    pubkey = %pubkey,
                    ?reason,
                    "Removed final delegated-account subscription ownership \
                     and LRU entry silently; no removal notification emitted"
                );
                Ok(true)
            }
            Err(err) => {
                if matches!(
                    err,
                    RemoteAccountProviderError::AccountSubscriptionDoesNotExist(
                        _
                    )
                ) {
                    self.lrucache_subscribed_accounts.remove(pubkey);
                    trace!(
                        pubkey = %pubkey,
                        ?reason,
                        "Removed stale delegated-account LRU entry for missing subscription"
                    );
                    return Ok(false);
                }

                let mut ownership = self.subscription_ownership.lock().await;
                if ownership
                    .get(pubkey)
                    .is_none_or(SubscriptionOwnership::is_empty)
                {
                    let ownership = ownership.entry(*pubkey).or_default();
                    for _ in 0..released_count {
                        ownership.acquire(reason);
                    }
                }
                drop(ownership);

                Err(err)
            }
        }
    }

    /// Subscribe to program account updates
    #[instrument(skip(self))]
    pub async fn subscribe_program(
        &self,
        program_id: Pubkey,
    ) -> RemoteAccountProviderResult<()> {
        self.pubsub_client.subscribe_program(program_id).await
    }

    /// Get a reference to the pubsub client (for testing)
    #[cfg(any(test, feature = "dev-context"))]
    pub fn pubsub_client(&self) -> &U {
        &self.pubsub_client
    }

    /// Unsubscribe from an account
    #[instrument(skip(self))]
    pub async fn unsubscribe(
        &self,
        pubkey: &Pubkey,
    ) -> RemoteAccountProviderResult<()> {
        let _transition_guard = self.subscription_transition_lock.lock().await;
        let subscription_key_lock = self.subscription_key_lock(pubkey).await;
        let _subscription_guard = subscription_key_lock.lock().await;

        if !self.lrucache_subscribed_accounts.can_evict(pubkey) {
            warn!(pubkey = %pubkey, "Tried to unsubscribe from account that should never be evicted");
            return Ok(());
        }

        if !self.lrucache_subscribed_accounts.contains(pubkey) {
            trace!(pubkey = %pubkey, "Already unsubscribed from LRU");
            return Ok(());
        }

        let success = subscription_reconciler::unsubscribe_and_notify_removal(
            *pubkey,
            &self.pubsub_client,
            &self.removed_account_tx,
        )
        .await;

        if success {
            self.lrucache_subscribed_accounts.remove(pubkey);
            self.subscription_ownership.lock().await.remove(pubkey);
        }

        Ok(())
    }

    /// Tries to fetch the given accounts from RPC.
    /// NOTE: if we get an RPC error we just log it and give up since there is no
    ///       obvious way how to handle this even if we were to bubble the error up.
    /// Any action that depends on those accounts to be there will fail.
    /// NOTE: this is not used during subscription updates since we receive the data
    ///       as part of that update, thus we won't have stale data issues.
    #[allow(clippy::too_many_arguments)]
    fn fetch(
        &self,
        pubkeys: Vec<Pubkey>,
        generations: HashMap<Pubkey, FetchingAccountGeneration>,
        mark_empty_if_not_found: Option<&[Pubkey]>,
        min_context_slot: u64,
        fetch_origin: AccountFetchOrigin,
    ) {
        let rpc_client = self.rpc_client.clone();
        let fetching_accounts = self.fetching_accounts.clone();
        let commitment = self.rpc_client.commitment();
        let mark_empty_if_not_found =
            mark_empty_if_not_found.unwrap_or(&[]).to_vec();
        tokio::spawn(async move {
            use RemoteAccount::*;

            let fetch_started_at = std::time::Instant::now();
            // Helper to notify all pending requests of fetch failure
            let notify_error = |error_msg: &str| {
                let mut fetching = fetching_accounts.lock().unwrap();
                warn!(
                    pubkey_count = pubkeys.len(),
                    pubkeys = %pubkeys_str(&pubkeys),
                    min_context_slot = min_context_slot,
                    commitment = ?commitment,
                    fetch_origin = %fetch_origin,
                    elapsed_ms = fetch_started_at.elapsed().as_millis() as u64,
                    error = %error_msg,
                    "{error_msg}"
                );
                inc_account_fetches_failed(pubkeys.len() as u64);

                for pubkey in &pubkeys {
                    // Update metrics
                    // Remove pending requests and send error
                    if let Some(generation) = generations.get(pubkey).copied() {
                        if let Some(state) =
                            remove_fetching_account_if_generation_matches(
                                &mut fetching,
                                pubkey,
                                generation,
                            )
                        {
                            for sender in state.waiters {
                                let error = RemoteAccountProviderError::AccountResolutionsFailed(
                                    format!("{}: {}", pubkey, error_msg)
                                );
                                let _ = sender.send(Err(error));
                            }
                        }
                    }
                }
            };

            let mut remaining_retries: u64 = RPC_FETCH_MAX_RETRIES;

            if tracing::enabled!(tracing::Level::TRACE) {
                trace!(pubkeys = pubkeys_str(&pubkeys), "Fetching accounts");
            }

            macro_rules! retry {
                ($msg:expr) => {{
                    trace!($msg);
                    remaining_retries -= 1;
                    if remaining_retries <= 0 {
                        let err_msg = format!("Max retries {RPC_FETCH_MAX_RETRIES} reached, giving up on fetching accounts: {pubkeys:?}");
                        notify_error(&err_msg);
                        return;
                    }
                    tokio::time::sleep(RPC_FETCH_RETRY_DELAY).await;
                    continue;
                }};
            }
            let response = loop {
                // We provide the min_context slot in order to _force_ the RPC to update
                // its account cache. Otherwise we could just keep fetching the accounts
                // until the context slot is high enough.
                metrics::inc_remote_account_provider_a_count();
                match tokio::time::timeout(RPC_FETCH_TIMEOUT, async {
                    let config = RpcAccountInfoConfig {
                        commitment: Some(commitment),
                        min_context_slot: Some(min_context_slot),
                        encoding: Some(UiAccountEncoding::Base64Zstd),
                        data_slice: None,
                    };

                    if pubkeys.len() == 1 {
                        rpc_client
                            .get_account_with_config(&pubkeys[0], config)
                            .await
                            .map(|res| (res.context.slot, vec![res.value]))
                    } else {
                        rpc_client
                            .get_multiple_accounts_with_config(&pubkeys, config)
                            .await
                            .map(|res| (res.context.slot, res.value))
                    }
                })
                .await
                {
                    Ok(Ok(res)) => {
                        let (slot, value) = res;
                        if slot < min_context_slot {
                            retry!("Response slot {slot} < {min_context_slot}. Retrying...");
                        } else {
                            break (slot, value);
                        }
                    }
                    Ok(Err(err)) => match *err.kind {
                        ErrorKind::RpcError(rpc_err) => {
                            match rpc_err {
                                RpcError::ForUser(ref rpc_user_err) => {
                                    // When an account is not present for the desired
                                    // min-context slot then we normally get the below
                                    // handled `RpcResponseError`, but may also get the
                                    // following error from the RPC.
                                    // See test::ixtest_existing_account_for_future_slot
                                    // ```
                                    // RpcError(
                                    //   ForUser(
                                    //       "AccountNotFound: \
                                    // pubkey=DaeruQ4SukTQaJA5muyv51MQZok7oaCAF8fAW19mbJv5: \
                                    //        RPC response error -32016: \
                                    //        Minimum context slot has not been reached; ",
                                    //   ),
                                    // )
                                    // ```
                                    retry!("Fetching accounts failed: {rpc_user_err:?}");
                                 }
                                RpcError::RpcResponseError {
                                    code,
                                    message,
                                    data,
                                } => {
                                    if code == JSON_RPC_SERVER_ERROR_MIN_CONTEXT_SLOT_NOT_REACHED || code == HELIUS_CONTEXT_SLOT_NOT_REACHED {
                                        retry!("Minimum context slot {min_context_slot} not reached for {commitment:?}. code={code}, message={message}, data={data:?}");
                                    } else {
                                        let err = RpcError::RpcResponseError {
                                            code,
                                            message,
                                            data,
                                        };
                                        let err_msg = format!(
                                            "RpcError fetching accounts {}: {err:?}", pubkeys_str(&pubkeys)
                                        );
                                        notify_error(&err_msg);
                                        return;
                                    }
                                }
                                err => {
                                    let err_msg = format!(
                                        "RpcError fetching accounts {}: {err:?}", pubkeys_str(&pubkeys)
                                    );
                                     notify_error(&err_msg);
                                     return;
                                 }
                            }
                        }
                        ErrorKind::Custom(message)
                            if message
                                .to_ascii_lowercase()
                                .contains("minimum context slot") =>
                        {
                            retry!(
                                "Minimum context slot {min_context_slot} not reached for {commitment:?}: {message}"
                            );
                        }
                        _ => {
                            let err_msg = format!(
                                "RpcError fetching accounts {}: {err:?}",
                                pubkeys_str(&pubkeys)
                            );
                            notify_error(&err_msg);
                            return;
                        }
                    },
                    Err(_) => {
                        let attempt =
                            RPC_FETCH_MAX_RETRIES - remaining_retries + 1;
                        warn!(
                            pubkey_count = pubkeys.len(),
                            pubkeys = %pubkeys_str(&pubkeys),
                            attempt = attempt,
                            max_retries = RPC_FETCH_MAX_RETRIES,
                            remaining_retries = remaining_retries.saturating_sub(1),
                            timeout_ms = RPC_FETCH_TIMEOUT.as_millis() as u64,
                            elapsed_ms = fetch_started_at.elapsed().as_millis() as u64,
                            min_context_slot = min_context_slot,
                            commitment = ?commitment,
                            fetch_origin = %fetch_origin,
                            "RPC call timeout. Retrying..."
                        );
                        remaining_retries -= 1;
                        if remaining_retries == 0 {
                            let err_msg = format!("Max retries {RPC_FETCH_MAX_RETRIES} reached, giving up on fetching accounts: {pubkeys:?}");
                            notify_error(&err_msg);
                            return;
                        }
                        tokio::time::sleep(RPC_FETCH_RETRY_DELAY).await;
                        continue;
                    }
                };
            };

            // TODO: should we retry if not or respond with an error?
            let (response_slot, response_value) = response;
            assert!(response_slot >= min_context_slot);

            if response_value.len() != pubkeys.len() {
                let err_msg = format!(
                    "RPC returned {} account results for {} requested accounts: {}",
                    response_value.len(),
                    pubkeys.len(),
                    pubkeys_str(&pubkeys)
                );
                notify_error(&err_msg);
                return;
            }

            let mut found_count = 0u64;
            let mut not_found_count = 0u64;

            let remote_accounts: Vec<RemoteAccount> = pubkeys
                .iter()
                .zip(response_value)
                .map(|(pubkey, acc)| match acc {
                    Some(value) => {
                        found_count += 1;
                        RemoteAccount::from_fresh_account(
                            value,
                            response_slot,
                            RemoteAccountUpdateSource::Fetch,
                        )
                    }
                    None if mark_empty_if_not_found.contains(pubkey) => {
                        not_found_count += 1;
                        RemoteAccount::from_fresh_account(
                            Account {
                                lamports: 0,
                                data: vec![],
                                owner: Pubkey::default(),
                                executable: false,
                                rent_epoch: 0,
                            },
                            response_slot,
                            RemoteAccountUpdateSource::Fetch,
                        )
                    }
                    None => {
                        not_found_count += 1;
                        NotFound(response_slot)
                    }
                })
                .collect();

            // Update metrics for successful RPC fetch
            inc_account_fetches_success(pubkeys.len() as u64);
            inc_account_fetches_found(fetch_origin, found_count);
            inc_account_fetches_not_found(fetch_origin, not_found_count);

            if tracing::enabled!(tracing::Level::TRACE) {
                let pubkeys = pubkeys
                    .iter()
                    .map(|pk| pk.to_string())
                    .collect::<Vec<_>>()
                    .join(", ");
                trace!(
                    pubkeys = %pubkeys, remote_accounts = ?remote_accounts, "Fetched, notifying pending requests"
                );
            }

            // Notify all pending requests with fetch results (unless subscription override occurred)
            for (pubkey, remote_account) in
                pubkeys.iter().zip(remote_accounts.iter())
            {
                let waiters = {
                    let mut fetching = fetching_accounts.lock().unwrap();
                    // Remove from fetching and get pending requests
                    // Note: the account might have been resolved by a
                    // subscription update already or replaced by a newer owner.
                    let Some(generation) = generations.get(pubkey).copied()
                    else {
                        continue;
                    };
                    if let Some(state) =
                        remove_fetching_account_if_generation_matches(
                            &mut fetching,
                            pubkey,
                            generation,
                        )
                    {
                        state.waiters
                    } else {
                        // Account was already resolved or replaced, skip.
                        if tracing::enabled!(tracing::Level::TRACE) {
                            trace!(
                                "Account {pubkey} generation {generation} was already resolved or replaced"
                            );
                        }
                        continue;
                    }
                };

                // Send the fetch result to all waiting requests
                for request in waiters {
                    let _ = request.send(Ok(remote_account.clone()));
                }
            }
        });
    }
}

fn remove_fetching_account_if_generation_matches(
    fetching: &mut HashMap<Pubkey, FetchingAccountState>,
    pubkey: &Pubkey,
    generation: FetchingAccountGeneration,
) -> Option<FetchingAccountState> {
    match fetching.entry(*pubkey) {
        Entry::Occupied(entry) if entry.get().generation == generation => {
            Some(entry.remove())
        }
        _ => None,
    }
}

impl RemoteAccountProvider<ChainRpcClientImpl, ChainPubsubClientImpl> {
    #[cfg(any(test, feature = "dev-context"))]
    pub fn rpc_client(&self) -> &RpcClient {
        &self.rpc_client.rpc_client
    }
}

impl
    RemoteAccountProvider<
        ChainRpcClientImpl,
        SubMuxClient<ChainPubsubClientImpl>,
    >
{
    #[cfg(any(test, feature = "dev-context"))]
    pub fn rpc_client(&self) -> &RpcClient {
        &self.rpc_client.rpc_client
    }
}

impl
    RemoteAccountProvider<ChainRpcClientImpl, SubMuxClient<ChainUpdatesClient>>
{
    #[cfg(any(test, feature = "dev-context"))]
    pub fn rpc_client(&self) -> &RpcClient {
        &self.rpc_client.rpc_client
    }
}

fn all_slots_match(accs: &[RemoteAccount]) -> bool {
    if accs.is_empty() {
        return true;
    }
    let slot = accs.first().unwrap().slot();
    accs.iter().all(|acc| acc.slot() == slot)
}

enum SlotsMatchResult {
    Match,
    Mismatch,
    MatchButBelowMinContextSlot(u64),
}

fn slots_match_and_meet_min_context(
    accs: &[RemoteAccount],
    min_context_slot: Option<u64>,
) -> SlotsMatchResult {
    if !all_slots_match(accs) {
        return SlotsMatchResult::Mismatch;
    }

    if let Some(min_slot) = min_context_slot {
        let respect_slot = accs
            .first()
            .is_none_or(|first_acc| first_acc.slot() >= min_slot);
        if respect_slot {
            SlotsMatchResult::Match
        } else {
            SlotsMatchResult::MatchButBelowMinContextSlot(min_slot)
        }
    } else {
        SlotsMatchResult::Match
    }
}

fn account_slots(accs: &[RemoteAccount]) -> Vec<u64> {
    accs.iter().map(|acc| acc.slot()).collect()
}

fn pubkeys_str(pubkeys: &[Pubkey]) -> String {
    pubkeys
        .iter()
        .map(|pk| pk.to_string())
        .collect::<Vec<_>>()
        .join(", ")
}
