use std::{
    collections::{HashMap, HashSet, hash_map::Entry},
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicU64, Ordering},
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
use futures_util::future::{join_all, try_join_all};
use magicblock_config::config::{GrpcConfig, SubscriptionTransport};
pub(crate) use remote_account::RemoteAccount;
pub use remote_account::RemoteAccountUpdateSource;
use solana_account::{Account, AccountBuilder, AccountMode};
use solana_account_decoder_client_types::{
    UiAccountEncoding, UiDataSliceConfig,
};
use solana_commitment_config::CommitmentConfig;
use solana_pubkey::Pubkey;
use solana_rpc_client_api::{
    client_error::ErrorKind,
    config::{RpcAccountInfoConfig, RpcProgramAccountsConfig},
    custom_error::JSON_RPC_SERVER_ERROR_MIN_CONTEXT_SLOT_NOT_REACHED,
    request::RpcError,
};
use solana_sdk_ids::sysvar::clock;
pub use subscribed_accounts::SubscribedAccounts;
use tokio::{
    sync::{Mutex as AsyncMutex, Notify, mpsc, oneshot},
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
pub mod program_account;
mod provider_fetch;
mod provider_setup;
mod provider_subscriptions;
mod provider_updates;
pub mod pubsub_common;
pub mod pubsub_connection;
pub mod pubsub_connection_pool;
mod remote_account;
mod subscribed_accounts;
pub(crate) mod subscription_reconciler;

#[cfg(test)]
mod tests;

pub use endpoint::{Endpoint, Endpoints};
use magicblock_metrics::{
    metrics,
    metrics::{
        AccountFetchContext, AccountFetchReason, ChainlinkCompanionFetchKind,
        ChainlinkCompanionFetchOutcome, ChainlinkEmptyPlaceholderStage,
        ChainlinkPendingFetchLayer, ChainlinkPendingFetchOutcome, Outcome,
        SubscriptionCleanupOutcome, SubscriptionCleanupSource,
        SubscriptionReasonLabel, SubscriptionRegistrationOrigin,
        SubscriptionRegistrationOutcome, SubscriptionReleaseOutcome,
        dec_chainlink_pending_fetch_waiters_gauge, inc_account_fetches_failed,
        inc_account_fetches_found_with_context,
        inc_account_fetches_not_found_with_context,
        inc_account_fetches_success,
        inc_chainlink_empty_placeholder_accounts_total_with_context,
        inc_chainlink_pending_fetch_accounts_with_context,
        inc_chainlink_pending_fetch_waiters_gauge,
        inc_chainlink_pending_fetch_waiters_with_context,
        inc_chainlink_subscription_cleanup_accounts,
        inc_chainlink_subscription_registration_accounts,
        inc_chainlink_subscription_release_accounts,
        observe_chainlink_companion_fetch_attempts,
        observe_chainlink_companion_fetch_duration_seconds,
        observe_chainlink_pending_fetch_owner_duration_seconds_with_context,
        set_monitored_accounts_count,
    },
};
pub use remote_account::ResolvedAccount;

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

type SubscriptionKeyLocks =
    Arc<AsyncMutex<HashMap<Pubkey, Weak<AsyncMutex<()>>>>>;

pub(crate) async fn subscription_key_lock_from_map(
    subscription_key_locks: &SubscriptionKeyLocks,
    pubkey: &Pubkey,
) -> Arc<AsyncMutex<()>> {
    let mut locks = subscription_key_locks.lock().await;
    locks.retain(|_, lock| lock.strong_count() > 0);

    if let Some(lock) = locks.get(pubkey).and_then(Weak::upgrade) {
        return lock;
    }

    let lock = Arc::new(AsyncMutex::new(()));
    locks.insert(*pubkey, Arc::downgrade(&lock));
    lock
}

pub(crate) async fn subscription_key_owned_guard_from_map(
    subscription_key_locks: &SubscriptionKeyLocks,
    pubkey: Pubkey,
) -> tokio::sync::OwnedMutexGuard<()> {
    // The reconciler uses this to serialize repair work with normal
    // acquire/release/unsubscribe transitions for the same pubkey. Creating the
    // lock when it is missing is intentional: if reconciliation only looked up
    // existing locks, a new same-pubkey transition could start immediately after
    // the lookup and race the repair. Reconciliation only calls this for drifted
    // pubkeys it is about to repair, not for every subscribed account.
    let lock =
        subscription_key_lock_from_map(subscription_key_locks, &pubkey).await;
    lock.lock_owned().await
}

type ChainUpdatesPubsub = (Arc<ChainUpdatesClient>, mpsc::Receiver<()>);

#[allow(clippy::too_many_arguments)]
async fn connect_pubsub_client(
    ep: Endpoint,
    commitment: CommitmentConfig,
    rpc_client: ChainRpcClientImpl,
    chain_slot: Arc<AtomicU64>,
    resubscription_delay: Duration,
    ws_subs_per_connection: Option<usize>,
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
        ws_subs_per_connection,
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

// Maps pubkey -> (fetch_start_slot, requests_waiting)
type FetchResult = Result<RemoteAccount, RemoteAccountProviderError>;
type FetchingAccountGeneration = u64;

struct FetchingAccountState {
    generation: FetchingAccountGeneration,
    fetch_start_slot: u64,
    fetch_context: AccountFetchContext,
    owner_started_at: std::time::Instant,
    waiters: Vec<oneshot::Sender<FetchResult>>,
}

type FetchingAccounts = Mutex<HashMap<Pubkey, FetchingAccountState>>;

struct PendingFetchWaiterGaugeGuard {
    layer: ChainlinkPendingFetchLayer,
    active: bool,
}

impl PendingFetchWaiterGaugeGuard {
    fn active(layer: ChainlinkPendingFetchLayer) -> Self {
        Self {
            layer,
            active: true,
        }
    }

    fn inactive(layer: ChainlinkPendingFetchLayer) -> Self {
        Self {
            layer,
            active: false,
        }
    }

    fn finish(&mut self) {
        if self.active {
            dec_chainlink_pending_fetch_waiters_gauge(self.layer);
            self.active = false;
        }
    }
}

impl Drop for PendingFetchWaiterGaugeGuard {
    fn drop(&mut self) {
        self.finish();
    }
}

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
                    observe_chainlink_pending_fetch_owner_duration_seconds_with_context(
                        state.fetch_context,
                        ChainlinkPendingFetchLayer::RemoteAccountProvider,
                        ChainlinkPendingFetchOutcome::OwnerFailed,
                        state.owner_started_at.elapsed().as_secs_f64(),
                    );
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
/// `DirectAccount` is normal remote-account monitoring.
/// `UndelegationTracking` keeps monitoring delegated accounts while they are
/// being undelegated, even after direct ownership is released.
///
/// Delegated accounts that are not undelegating are locally authoritative and
/// should have `DirectAccount` ownership released once delegation is discovered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SubscriptionReason {
    DirectAccount,
    DelegationRecord,
    ProgramData,
    UndelegationTracking,
    AtaProjection,
}

impl From<SubscriptionReason> for SubscriptionReasonLabel {
    fn from(reason: SubscriptionReason) -> Self {
        match reason {
            SubscriptionReason::DirectAccount => Self::DirectAccount,
            SubscriptionReason::DelegationRecord => Self::DelegationRecord,
            SubscriptionReason::ProgramData => Self::ProgramData,
            SubscriptionReason::UndelegationTracking => {
                Self::UndelegationTracking
            }
            SubscriptionReason::AtaProjection => Self::AtaProjection,
        }
    }
}

pub(crate) type SubscriptionOwnershipMap =
    Arc<AsyncMutex<HashMap<Pubkey, SubscriptionOwnership>>>;

#[derive(Debug, Default, Clone)]
pub(crate) struct SubscriptionOwnership {
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

#[derive(Clone)]
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
// Retries must ride out the RPC lagging the pubsub tip by several seconds (15 = ~5.6s),
// otherwise one-shot subscription updates (e.g. program upgrades) could be dropped
const RPC_FETCH_MAX_RETRIES: u64 = 15;
const RPC_FETCH_RETRY_DELAY: Duration = Duration::from_millis(400);
/// Attempts for a `dataSlice` fetch to reach the required context slot
/// before giving up; callers fall back to a full fetch.
const DATA_SLICE_FETCH_MAX_ATTEMPTS: usize = 5;
const RPC_FETCH_TIMEOUT: Duration = Duration::from_secs(5);
const MATCH_SLOTS_MAX_TOTAL_TIME: Duration = Duration::from_secs(10);

// getMultipleAccounts accepts at most this many keys per request.
const MAX_MULTIPLE_ACCOUNTS_PER_REQUEST: usize = 100;

// Splits keys into the minimum number of chunks that fit the RPC's
// getMultipleAccounts limit, sized as evenly as possible.
fn balanced_chunks(keys: Vec<Pubkey>) -> Vec<Vec<Pubkey>> {
    if keys.len() <= MAX_MULTIPLE_ACCOUNTS_PER_REQUEST {
        return vec![keys];
    }
    let num_chunks = keys.len().div_ceil(MAX_MULTIPLE_ACCOUNTS_PER_REQUEST);
    let chunk_size = keys.len().div_ceil(num_chunks);
    keys.chunks(chunk_size)
        .map(|chunk| chunk.to_vec())
        .collect()
}

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
    /// Per-pubkey locks serializing subscription acquire/release transitions.
    ///
    /// Values are weak references so pubkeys do not accumulate forever after
    /// their transient transition lock is no longer in use.
    subscription_key_locks: SubscriptionKeyLocks,
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
    subscribed_accounts: Arc<SubscribedAccounts>,

    /// Accounts whose remote subscription failed and whose local readonly
    /// state must be discarded before it can be fetched again.
    stale_account_tx: mpsc::Sender<Pubkey>,
    stale_account_rx: Mutex<Option<mpsc::Receiver<Pubkey>>>,

    subscription_forwarder: Arc<mpsc::Sender<ForwardedSubscriptionUpdate>>,
    /// Per-account latest replay of consumed subscription results, drained
    /// losslessly by a dedicated worker (newest slot wins).
    replay_outbox: Arc<Mutex<HashMap<Pubkey, ForwardedSubscriptionUpdate>>>,
    replay_notify: Arc<Notify>,

    /// Task that periodically reconciles subscriptions and updates the
    /// active subscriptions gauge
    _active_subscriptions_task_handle: Option<task::JoinHandle<()>>,
}

impl<T: ChainRpcClient, U: ChainPubsubClient> Drop
    for RemoteAccountProvider<T, U>
{
    fn drop(&mut self) {
        // The reconciler loops forever; abort it so a dropped provider
        // doesn't leak the task and the state it holds
        if let Some(handle) = &self._active_subscriptions_task_handle {
            handle.abort();
        }
    }
}

// -----------------
// Configs
// -----------------
const DEFAULT_MATCH_SLOTS_MAX_RETRIES: u64 = 10;
const DEFAULT_MATCH_SLOTS_RETRY_INTERVAL_MS: u64 = 50;

pub struct MatchSlotsConfig {
    pub max_retries: u64,
    pub retry_interval_ms: u64,
    pub min_context_slot: Option<u64>,
    pub companion_fetch_kind: ChainlinkCompanionFetchKind,
}

impl MatchSlotsConfig {
    pub fn new(companion_fetch_kind: ChainlinkCompanionFetchKind) -> Self {
        Self {
            max_retries: DEFAULT_MATCH_SLOTS_MAX_RETRIES,
            retry_interval_ms: DEFAULT_MATCH_SLOTS_RETRY_INTERVAL_MS,
            min_context_slot: None,
            companion_fetch_kind,
        }
    }
}

struct MatchSlotsRetryConfig {
    max_retries: u64,
    retry_interval_ms: u64,
    min_context_slot: Option<u64>,
}

impl Default for MatchSlotsRetryConfig {
    fn default() -> Self {
        Self {
            max_retries: DEFAULT_MATCH_SLOTS_MAX_RETRIES,
            retry_interval_ms: DEFAULT_MATCH_SLOTS_RETRY_INTERVAL_MS,
            min_context_slot: None,
        }
    }
}

impl From<&MatchSlotsConfig> for MatchSlotsRetryConfig {
    fn from(config: &MatchSlotsConfig) -> Self {
        Self {
            max_retries: config.max_retries,
            retry_interval_ms: config.retry_interval_ms,
            min_context_slot: config.min_context_slot,
        }
    }
}

fn next_match_slots_retry(
    retries: &mut u64,
    start: std::time::Instant,
    config: &MatchSlotsRetryConfig,
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
    config: &MatchSlotsRetryConfig,
) -> Result<Duration, String> {
    next_match_slots_retry(retries, start, config)
        .map(|delay| delay.max(RPC_FETCH_RETRY_DELAY))
}

fn match_slots_retry_delay(config: &MatchSlotsRetryConfig) -> Duration {
    Duration::from_millis(config.retry_interval_ms)
}

fn observe_companion_fetch_if_configured(
    context: AccountFetchContext,
    kind: Option<ChainlinkCompanionFetchKind>,
    outcome: ChainlinkCompanionFetchOutcome,
    attempts: u64,
    started_at: std::time::Instant,
) {
    if let Some(kind) = kind {
        observe_chainlink_companion_fetch_attempts(
            context.clone(),
            kind,
            outcome,
            attempts as f64,
        );
        observe_chainlink_companion_fetch_duration_seconds(
            context.clone(),
            kind,
            outcome,
            started_at.elapsed().as_secs_f64(),
        );
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

/// Raises the min-context floor to the highest slot of any found account:
/// state observed at slot S must never be superseded by an older view.
fn raised_min_context_slot(
    min_context_slot: Option<u64>,
    accs: &[RemoteAccount],
) -> Option<u64> {
    let max_found_slot = accs
        .iter()
        .filter(|acc| acc.is_found())
        .map(|acc| acc.slot())
        .max();
    match (min_context_slot, max_found_slot) {
        (Some(min), Some(found)) => Some(min.max(found)),
        (None, Some(found)) => Some(found),
        (min, None) => min,
    }
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

#[cfg(any(test, feature = "dev-context"))]
impl<T: ChainRpcClient, U: ChainPubsubClient> RemoteAccountProvider<T, U> {
    /// Get a reference to the pubsub client for tests and dev tooling.
    pub fn pubsub_client(&self) -> &U {
        &self.pubsub_client
    }
}

impl<T: ChainRpcClient, U: ChainPubsubClient> RemoteAccountProvider<T, U> {
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

    pub(crate) async fn has_any_subscription_reason<'a, I>(
        &self,
        pubkeys: I,
        reason: SubscriptionReason,
    ) -> bool
    where
        I: IntoIterator<Item = &'a Pubkey>,
    {
        let subscription_ownership = self.subscription_ownership.lock().await;
        pubkeys.into_iter().any(|pubkey| {
            subscription_ownership
                .get(pubkey)
                .is_some_and(|ownership| ownership.contains(reason))
        })
    }
}

#[cfg(test)]
impl<T: ChainRpcClient, U: ChainPubsubClient> RemoteAccountProvider<T, U> {
    /// Check if an account is currently pending (being fetched).
    pub(crate) fn is_pending(&self, pubkey: &Pubkey) -> bool {
        let fetching = self
            .fetching_accounts
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        fetching.contains_key(pubkey)
    }
}

#[cfg(any(test, feature = "dev-context"))]
impl RemoteAccountProvider<ChainRpcClientImpl, ChainPubsubClientImpl> {
    pub fn rpc_client(
        &self,
    ) -> &solana_rpc_client::nonblocking::rpc_client::RpcClient {
        &self.rpc_client.rpc_client
    }
}

#[cfg(any(test, feature = "dev-context"))]
impl
    RemoteAccountProvider<
        ChainRpcClientImpl,
        SubMuxClient<ChainPubsubClientImpl>,
    >
{
    pub fn rpc_client(
        &self,
    ) -> &solana_rpc_client::nonblocking::rpc_client::RpcClient {
        &self.rpc_client.rpc_client
    }
}

#[cfg(any(test, feature = "dev-context"))]
impl
    RemoteAccountProvider<ChainRpcClientImpl, SubMuxClient<ChainUpdatesClient>>
{
    pub fn rpc_client(
        &self,
    ) -> &solana_rpc_client::nonblocking::rpc_client::RpcClient {
        &self.rpc_client.rpc_client
    }
}
