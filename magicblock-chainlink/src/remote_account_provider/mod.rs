use std::{
    collections::{hash_map::Entry, HashMap, HashSet},
    num::NonZeroUsize,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex, RwLock, Weak,
    },
    time::Duration,
};

pub(crate) use chain_pubsub_client::{
    ChainPubsubClient, ChainPubsubClientImpl, PubsubTransport,
    ReconnectableClient,
};
pub(crate) use chain_rpc_client::{ChainRpcClient, ChainRpcClientImpl};
use config::RemoteAccountProviderConfig;
pub(crate) use errors::{
    RemoteAccountProviderError, RemoteAccountProviderResult,
};
use futures_util::future::{join_all, try_join_all};
use lru_cache::TieredSubscribedAccountsTracker;
pub use lru_cache::{AccountsLruCache, AddAccountOutcome};
use magicblock_config::config::GrpcConfig;
pub(crate) use remote_account::RemoteAccount;
pub use remote_account::RemoteAccountUpdateSource;
use solana_account::Account;
use solana_account_decoder_client_types::UiAccountEncoding;
use solana_commitment_config::CommitmentConfig;
use solana_pubkey::Pubkey;
use solana_rpc_client_api::{
    client_error::ErrorKind,
    config::{RpcAccountInfoConfig, RpcProgramAccountsConfig},
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
pub(crate) mod subscription_reconciler;

#[cfg(test)]
mod tests;

pub use endpoint::{Endpoint, Endpoints};
use magicblock_metrics::{
    metrics,
    metrics::{
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
        set_monitored_accounts_count, AccountFetchContext, AccountFetchReason,
        ChainlinkCompanionFetchKind, ChainlinkCompanionFetchOutcome,
        ChainlinkEmptyPlaceholderStage, ChainlinkPendingFetchLayer,
        ChainlinkPendingFetchOutcome, Outcome, SubscriptionCleanupOutcome,
        SubscriptionCleanupSource, SubscriptionReasonLabel,
        SubscriptionRegistrationOrigin, SubscriptionRegistrationOutcome,
        SubscriptionReleaseOutcome,
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
    subscribed_accounts: Arc<TieredSubscribedAccountsTracker>,
) {
    // Deferred clients are the fallback update sources; retry connect/attach
    // with capped backoff instead of dropping them permanently on failure.
    const INITIAL_ATTACH_RETRY_DELAY: Duration = Duration::from_secs(1);
    const MAX_ATTACH_RETRY_DELAY: Duration = Duration::from_secs(300);
    for ep in endpoints {
        let rpc_client = rpc_client.clone();
        let chain_slot = chain_slot.clone();
        let grpc_cfg = grpc_cfg.clone();
        let submux = submux.clone();
        let subscribed_accounts = subscribed_accounts.clone();
        // Stop retrying when the owning mux shuts down so an unavailable
        // endpoint doesn't leak this task past the provider's lifetime
        let shutdown = submux.shutdown_token();
        tokio::spawn(async move {
            let mut retry_delay = INITIAL_ATTACH_RETRY_DELAY;
            loop {
                let connect = connect_pubsub_client(
                    ep.clone(),
                    commitment,
                    rpc_client.clone(),
                    chain_slot.clone(),
                    resubscription_delay,
                    grpc_cfg.clone(),
                );
                let (label, result) = tokio::select! {
                    _ = shutdown.cancelled() => return,
                    connected = connect => connected,
                };
                match result {
                    Ok((client, abort_rx)) => {
                        // Attach without the subscription transition lock:
                        // a paced resub of a large set can take minutes and
                        // would starve foreground transitions. The reconnect
                        // path already resubscribes lock-free.
                        let attach = submux.add_client(
                            client.clone(),
                            abort_rx,
                            subscribed_accounts.clone(),
                        );
                        let add_result = tokio::select! {
                            _ = shutdown.cancelled() => {
                                // Stop the partial client's subscription
                                // work; the mux is gone anyway
                                let _ = client.shutdown().await;
                                return;
                            }
                            res = attach => res,
                        };
                        match add_result {
                            Ok(()) => {
                                debug!(
                                    endpoint = %label,
                                    "Attached deferred pubsub client"
                                );
                                return;
                            }
                            Err(err) => {
                                warn!(
                                    endpoint = %label,
                                    error = %err,
                                    retry_in = ?retry_delay,
                                    "Deferred pubsub client failed to attach"
                                );
                                // Shut down so partially established
                                // subscriptions don't leak listener tasks
                                // before retrying with a fresh client
                                if let Err(err) = client.shutdown().await {
                                    debug!(
                                        endpoint = %label,
                                        error = %err,
                                        "Failed to shut down partially \
                                         attached pubsub client"
                                    );
                                }
                            }
                        }
                    }
                    Err(err) => warn!(
                        endpoint = %label,
                        error = %err,
                        retry_in = ?retry_delay,
                        "Deferred pubsub client failed to connect"
                    ),
                }
                tokio::select! {
                    _ = shutdown.cancelled() => return,
                    _ = tokio::time::sleep(retry_delay) => {}
                }
                retry_delay = (retry_delay * 2).min(MAX_ATTACH_RETRY_DELAY);
            }
        });
    }
}

// Maps pubkey -> (fetch_start_slot, requests_waiting)
type FetchResult = Result<RemoteAccount, RemoteAccountProviderError>;
type FetchingAccountGeneration = u64;

pub(crate) struct FetchingAccountState {
    generation: FetchingAccountGeneration,
    fetch_start_slot: u64,
    fetch_context: AccountFetchContext,
    owner_started_at: std::time::Instant,
    waiters: Vec<oneshot::Sender<FetchResult>>,
}

pub(crate) type FetchingAccounts = Mutex<HashMap<Pubkey, FetchingAccountState>>;

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
    subscription_ownership: SubscriptionOwnershipMap,
    subscription_transition_lock: Arc<AsyncMutex<()>>,
    primary: Arc<AccountsLruCache>,
    secondary: Arc<AccountsLruCache>,
    claimed_pubkeys: Vec<Pubkey>,
    claimed_generations: HashMap<Pubkey, FetchingAccountGeneration>,
    cancellation_error_text: Option<String>,
}

impl ClaimedSubscriptionSetupGuard {
    fn new(
        fetching_accounts: Arc<FetchingAccounts>,
        subscription_ownership: SubscriptionOwnershipMap,
        subscription_transition_lock: Arc<AsyncMutex<()>>,
        primary: Arc<AccountsLruCache>,
        secondary: Arc<AccountsLruCache>,
        claimed_pubkeys: Vec<Pubkey>,
        claimed_generations: HashMap<Pubkey, FetchingAccountGeneration>,
    ) -> Self {
        Self {
            fetching_accounts,
            subscription_ownership,
            subscription_transition_lock,
            primary,
            secondary,
            claimed_pubkeys,
            claimed_generations,
            cancellation_error_text: Some(
                "account subscription setup cancelled".to_string(),
            ),
        }
    }

    fn cleanup_fetching_with_error(&self, waiter_error_text: &str) {
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
                                waiter_error_text.to_string(),
                            ),
                        ));
                    }
                }
            }
        }
    }

    async fn cleanup_with_error(&mut self, waiter_error_text: String) {
        self.cleanup_fetching_with_error(&waiter_error_text);
        cleanup_classification_placeholders(
            &self.subscription_ownership,
            &self.subscription_transition_lock,
            &self.primary,
            &self.secondary,
            &self.claimed_generations,
        )
        .await;
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
        self.cleanup_fetching_with_error(&waiter_error_text);

        let subscription_ownership = self.subscription_ownership.clone();
        let subscription_transition_lock =
            self.subscription_transition_lock.clone();
        let primary = self.primary.clone();
        let secondary = self.secondary.clone();
        let claimed_generations = std::mem::take(&mut self.claimed_generations);
        task::spawn(async move {
            cleanup_classification_placeholders(
                &subscription_ownership,
                &subscription_transition_lock,
                &primary,
                &secondary,
                &claimed_generations,
            )
            .await;
        });
    }
}

async fn cleanup_classification_placeholders(
    subscription_ownership: &SubscriptionOwnershipMap,
    subscription_transition_lock: &Arc<AsyncMutex<()>>,
    primary: &AccountsLruCache,
    secondary: &AccountsLruCache,
    claimed_generations: &HashMap<Pubkey, FetchingAccountGeneration>,
) {
    let _transition_guard = subscription_transition_lock.lock().await;
    let mut ownership = subscription_ownership.lock().await;
    for (pubkey, generation) in claimed_generations {
        // Keep the placeholder when the key already holds tier state: the
        // update pump admitted it into the primary tier after winning fetch
        // arbitration, and dropping the ownership here would orphan that
        // membership. A later acquire (or capacity eviction) adopts it.
        if primary.contains(pubkey) || secondary.contains(pubkey) {
            continue;
        }
        if ownership.get(pubkey).is_some_and(|entry| {
            entry.is_empty()
                && entry.classification_placeholder_generation
                    == Some(*generation)
        }) {
            ownership.remove(pubkey);
        }
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SubscriptionClassificationSource {
    Fetch,
    Subscription,
}

#[derive(Debug, Clone, Copy)]
struct SubscriptionClassification {
    slot: u64,
    source: SubscriptionClassificationSource,
}

#[derive(Debug, Default, Clone)]
pub(crate) struct SubscriptionOwnership {
    reasons: HashMap<SubscriptionReason, usize>,
    last_classification: Option<SubscriptionClassification>,
    classification_placeholder_generation: Option<FetchingAccountGeneration>,
}

impl SubscriptionOwnership {
    fn acquire(&mut self, reason: SubscriptionReason) {
        self.classification_placeholder_generation = None;
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

/// Shared state for serialized movement between the primary and secondary
/// subscription tiers.
///
/// Locking rules:
/// - The per-key subscription guard is acquired first and may be held across
///   pubsub network calls for that key.
/// - `subscription_transition_lock` protects the composite in-memory tier
///   state (both LRUs, ownership map, confirmed-missing set). It is acquired
///   after the per-key guard, kept to short in-memory critical sections, and
///   MUST NOT be held across any pubsub subscribe/unsubscribe await.
/// - Cleanup of a key evicted by another key's admission runs as a detached
///   task ([Self::spawn_evicted_cleanup]) so no task ever holds two per-key
///   guards at once.
#[derive(Clone)]
struct SubscriptionTierCtx<U: ChainPubsubClient> {
    primary: Arc<AccountsLruCache>,
    secondary: Arc<AccountsLruCache>,
    pubsub_client: U,
    subscription_ownership:
        Arc<AsyncMutex<HashMap<Pubkey, SubscriptionOwnership>>>,
    subscription_transition_lock: Arc<AsyncMutex<()>>,
    subscription_key_locks: SubscriptionKeyLocks,
    fetching_accounts: Arc<FetchingAccounts>,
    capacity_eviction_protection: SharedCapacityEvictionProtectionPredicate,
    confirmed_missing_subscriptions: Arc<Mutex<HashSet<Pubkey>>>,
    removed_account_tx: mpsc::Sender<Pubkey>,
}

impl<U: ChainPubsubClient> SubscriptionTierCtx<U> {
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
            },
        )
    }

    fn is_confirmed_missing(&self, pubkey: &Pubkey) -> bool {
        self.confirmed_missing_subscriptions
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .contains(pubkey)
    }

    fn set_confirmed_missing(&self, pubkey: Pubkey, confirmed: bool) {
        let mut subscriptions = self
            .confirmed_missing_subscriptions
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if confirmed {
            subscriptions.insert(pubkey);
        } else {
            subscriptions.remove(&pubkey);
        }
    }

    async fn record_classification(
        &self,
        pubkey: Pubkey,
        slot: u64,
        source: SubscriptionClassificationSource,
    ) -> bool {
        let mut ownership = self.subscription_ownership.lock().await;
        let Some(ownership) = ownership.get_mut(&pubkey) else {
            return false;
        };

        Self::record_classification_entry(ownership, slot, source)
    }

    /// Like [Self::record_classification] but creates the ownership entry,
    /// which the pending fetch's in-flight acquisition adopts right after.
    async fn record_classification_for_pending_fetch(
        &self,
        pubkey: Pubkey,
        slot: u64,
        source: SubscriptionClassificationSource,
        generation: FetchingAccountGeneration,
    ) -> bool {
        let mut ownership = self.subscription_ownership.lock().await;
        let ownership = ownership.entry(pubkey).or_default();
        let apply_classification =
            Self::record_classification_entry(ownership, slot, source);
        if ownership.is_empty() {
            ownership.classification_placeholder_generation = Some(generation);
        }
        apply_classification
    }

    fn record_classification_entry(
        ownership: &mut SubscriptionOwnership,
        slot: u64,
        source: SubscriptionClassificationSource,
    ) -> bool {
        if ownership.last_classification.is_some_and(|last| {
            Self::classification_is_stale(last, slot, source)
        }) {
            return false;
        }

        ownership.last_classification =
            Some(SubscriptionClassification { slot, source });
        true
    }

    /// Records the RPC result's classification and applies the resulting tier
    /// movement when it is still current.
    async fn apply_fetch_classification(
        &self,
        pubkey: &Pubkey,
        response_slot: u64,
        not_found: bool,
    ) -> RemoteAccountProviderResult<()> {
        let apply_classification = self
            .record_classification(
                *pubkey,
                response_slot,
                SubscriptionClassificationSource::Fetch,
            )
            .await;
        if !apply_classification {
            return Ok(());
        }

        if not_found {
            self.move_not_found_to_secondary(*pubkey).await;
            Ok(())
        } else {
            // A confirmed miss that exists after all is gRPC-only; restore
            // full coverage on promotion.
            let restore_full_coverage = self.is_confirmed_missing(pubkey);
            self.set_confirmed_missing(*pubkey, false);
            match self
                .try_promote_found_to_primary(*pubkey, restore_full_coverage)
                .await
            {
                Ok(PromotionOutcome::NoCapacity) => {
                    self.finalize_rejected_promotion(pubkey).await;
                    Err(
                        RemoteAccountProviderError::NoEvictableSubscriptionCapacity {
                            pubkey: *pubkey,
                        },
                    )
                }
                // Evicted mid-promotion by another key's admission: the
                // detached eviction cleanup owns the state removal and bank
                // eviction; the found result must not be returned without
                // primary membership.
                Ok(PromotionOutcome::Evicted) => Err(
                    RemoteAccountProviderError::NoEvictableSubscriptionCapacity {
                        pubkey: *pubkey,
                    },
                ),
                other => other.map(|_| ()),
            }
        }
    }

    /// Finalizes a rejected secondary-tier promotion. The rejection decision
    /// is final, so the tier state, ownership, and bank entry are dropped
    /// even when the unsubscribe fails — the reconciler collects the stray
    /// subscription on a later pass. Keeping the state on unsubscribe
    /// failure would let the recorded found classification win arbitration
    /// against a later fetch and leak the account without primary admission.
    /// Precondition: the caller holds the key's subscription guard.
    async fn finalize_rejected_promotion(&self, pubkey: &Pubkey) {
        if let Err(err) = self.cleanup_rejected_subscription(*pubkey).await {
            warn!(
                pubkey = %pubkey,
                error = ?err,
                "Failed to unsubscribe rejected promotion; reconciler will remove the stray subscription"
            );
        }
        {
            let _transition_guard =
                self.subscription_transition_lock.lock().await;
            self.secondary.remove(pubkey);
            self.set_confirmed_missing(*pubkey, false);
            self.subscription_ownership.lock().await.remove(pubkey);
        }
        // The bank may hold a stale entry (e.g. an empty placeholder from
        // the confirmed-missing phase); evict it so a later ensure
        // refetches the account.
        self.spawn_removal_notification(*pubkey);
    }

    async fn classification_is_current(
        &self,
        pubkey: Pubkey,
        slot: u64,
        source: SubscriptionClassificationSource,
    ) -> bool {
        self.subscription_ownership
            .lock()
            .await
            .get(&pubkey)
            .and_then(|ownership| ownership.last_classification)
            .is_none_or(|last| {
                !Self::classification_is_stale(last, slot, source)
            })
    }

    fn classification_is_stale(
        last: SubscriptionClassification,
        slot: u64,
        source: SubscriptionClassificationSource,
    ) -> bool {
        slot < last.slot
            || (slot == last.slot
                && source == SubscriptionClassificationSource::Fetch
                && last.source
                    == SubscriptionClassificationSource::Subscription)
    }

    /// Adds `pubkey` to `cache` honoring eviction protection.
    /// Precondition: the caller holds `subscription_transition_lock`.
    async fn add_with_protection(
        &self,
        cache: &AccountsLruCache,
        pubkey: Pubkey,
    ) -> AddAccountOutcome {
        let ownership = self.subscription_ownership.lock().await;
        let fetching = self
            .fetching_accounts
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        cache.add_with_evict_filter(pubkey, |candidate| {
            cache.can_evict(candidate)
                && !fetching.contains_key(candidate)
                && !self
                    .capacity_eviction_protection_for(candidate)
                    .is_protected()
                && !ownership.get(candidate).is_some_and(|ownership| {
                    ownership.contains(SubscriptionReason::UndelegationTracking)
                })
        })
    }

    /// Whether `pubkey` could be admitted to `cache` (advisory pre-check).
    /// Precondition: the caller holds `subscription_transition_lock`.
    async fn has_capacity_with_protection(
        &self,
        cache: &AccountsLruCache,
        pubkey: &Pubkey,
    ) -> bool {
        let ownership = self.subscription_ownership.lock().await;
        let fetching = self
            .fetching_accounts
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        cache.can_add_with_evict_filter(pubkey, |candidate| {
            cache.can_evict(candidate)
                && !fetching.contains_key(candidate)
                && !self
                    .capacity_eviction_protection_for(candidate)
                    .is_protected()
                && !ownership.get(candidate).is_some_and(|ownership| {
                    ownership.contains(SubscriptionReason::UndelegationTracking)
                })
        })
    }

    /// Cleans up an account that was just evicted from a tier: drops its
    /// subscription and notifies upstream so it can be removed from the bank.
    ///
    /// Runs as a detached task on purpose:
    /// - the caller already holds the admitted key's per-key guard; taking
    ///   the evicted key's guard inline could ABBA-deadlock with a concurrent
    ///   transition admitting the evicted key,
    /// - the unsubscribe network call must not run under the transition lock.
    ///
    /// The task re-checks tier membership under the evicted key's guard and
    /// skips keys that were re-admitted (or have a pending fetch) in the
    /// meantime. If the unsubscribe fails the tier state stands and the
    /// reconciler removes the stray subscription on its next pass.
    fn spawn_evicted_cleanup(&self, evicted: Pubkey) {
        let ctx = self.clone();
        task::spawn(async move {
            {
                let _evicted_guard = subscription_key_owned_guard_from_map(
                    &ctx.subscription_key_locks,
                    evicted,
                )
                .await;

                let still_evicted = {
                    let _transition_guard =
                        ctx.subscription_transition_lock.lock().await;
                    let fetching = ctx
                        .fetching_accounts
                        .lock()
                        .unwrap_or_else(|poison| poison.into_inner());
                    !ctx.primary.contains(&evicted)
                        && !ctx.secondary.contains(&evicted)
                        && !fetching.contains_key(&evicted)
                };
                if !still_evicted {
                    inc_chainlink_subscription_cleanup_accounts(
                        SubscriptionCleanupSource::CapacityEviction,
                        SubscriptionCleanupOutcome::RetainedIntentionally,
                    );
                    return;
                }

                let cleanup_outcome = match ctx
                    .pubsub_client
                    .unsubscribe(evicted)
                    .await
                {
                    Ok(()) => SubscriptionCleanupOutcome::Unsubscribed,
                    Err(
                        RemoteAccountProviderError::AccountSubscriptionDoesNotExist(
                            _,
                        ),
                    ) => SubscriptionCleanupOutcome::AlreadyAbsent,
                    Err(err) => {
                        warn!(
                            evicted = %evicted,
                            error = ?err,
                            "Failed to unsubscribe evicted account; reconciler will remove the stray subscription"
                        );
                        SubscriptionCleanupOutcome::UnsubscribeFailed
                    }
                };
                inc_chainlink_subscription_cleanup_accounts(
                    SubscriptionCleanupSource::CapacityEviction,
                    cleanup_outcome,
                );

                let _transition_guard =
                    ctx.subscription_transition_lock.lock().await;
                ctx.subscription_ownership.lock().await.remove(&evicted);
                ctx.set_confirmed_missing(evicted, false);
            }
            // Send after dropping the per-key guard: the removal consumer
            // takes per-key guards itself, so sending into the bounded
            // channel while holding one could stall the removal pipeline.
            if let Err(err) = ctx.removed_account_tx.send(evicted).await {
                warn!(evicted = %evicted, error = ?err, "Failed to send removal update for evicted account");
                inc_chainlink_subscription_cleanup_accounts(
                    SubscriptionCleanupSource::CapacityEviction,
                    SubscriptionCleanupOutcome::RemovalUpdateFailed,
                );
            }
        });
    }

    /// Notifies the removal pipeline that `pubkey` lost its last watch, so a
    /// stale bank entry (e.g. an empty placeholder cloned while the account
    /// was confirmed missing) is evicted and a later ensure refetches it.
    /// Detached because the removal consumer takes per-key guards; sending
    /// inline while holding this key's guard could stall the pipeline.
    fn spawn_removal_notification(&self, pubkey: Pubkey) {
        let removed_account_tx = self.removed_account_tx.clone();
        task::spawn(async move {
            if let Err(err) = removed_account_tx.send(pubkey).await {
                warn!(pubkey = %pubkey, error = ?err, "Failed to send removal update for rejected promotion");
                inc_chainlink_subscription_cleanup_accounts(
                    SubscriptionCleanupSource::RejectedNewSubscription,
                    SubscriptionCleanupOutcome::RemovalUpdateFailed,
                );
            }
        });
    }

    async fn cleanup_rejected_subscription(
        &self,
        pubkey: Pubkey,
    ) -> RemoteAccountProviderResult<()> {
        match self.pubsub_client.unsubscribe(pubkey).await {
            Ok(()) => {
                inc_chainlink_subscription_cleanup_accounts(
                    SubscriptionCleanupSource::RejectedNewSubscription,
                    SubscriptionCleanupOutcome::Unsubscribed,
                );
                Ok(())
            }
            Err(
                RemoteAccountProviderError::AccountSubscriptionDoesNotExist(_),
            ) => {
                inc_chainlink_subscription_cleanup_accounts(
                    SubscriptionCleanupSource::RejectedNewSubscription,
                    SubscriptionCleanupOutcome::AlreadyAbsent,
                );
                Ok(())
            }
            Err(err) => {
                inc_chainlink_subscription_cleanup_accounts(
                    SubscriptionCleanupSource::RejectedNewSubscription,
                    SubscriptionCleanupOutcome::UnsubscribeFailed,
                );
                Err(err)
            }
        }
    }

    /// Registers `pubkey` in the secondary tier.
    /// Precondition: the caller holds the key's subscription guard; the
    /// transition lock is scoped internally and never spans the subscribe.
    async fn register_secondary(
        &self,
        pubkey: &Pubkey,
        reason: SubscriptionReason,
        origin: SubscriptionRegistrationOrigin,
    ) -> RemoteAccountProviderResult<()> {
        let has_capacity = {
            let _transition_guard =
                self.subscription_transition_lock.lock().await;
            self.has_capacity_with_protection(&self.secondary, pubkey)
                .await
        };
        if !has_capacity {
            inc_chainlink_subscription_registration_accounts(
                origin,
                reason.into(),
                SubscriptionRegistrationOutcome::RejectedNoCapacity,
            );
            return Err(
                RemoteAccountProviderError::NoEvictableSubscriptionCapacity {
                    pubkey: *pubkey,
                },
            );
        }

        // Keep full redundancy until the RPC result confirms the account is
        // missing; the confirming classification switches to gRPC-only
        // promptly and the reconciler repairs the policy on later passes.
        // Runs outside the transition lock; the per-key guard held by the
        // caller serializes transitions of this key.
        self.pubsub_client.subscribe(*pubkey, None).await?;

        let add_outcome = {
            let _transition_guard =
                self.subscription_transition_lock.lock().await;
            let add_outcome =
                self.add_with_protection(&self.secondary, *pubkey).await;
            if !matches!(add_outcome, AddAccountOutcome::NoEvictableCandidate) {
                self.set_confirmed_missing(*pubkey, false);
            }
            add_outcome
        };

        match add_outcome {
            AddAccountOutcome::AlreadyPresent => {
                inc_chainlink_subscription_registration_accounts(
                    origin,
                    reason.into(),
                    SubscriptionRegistrationOutcome::AlreadyPresent,
                );
            }
            AddAccountOutcome::Added => {
                inc_chainlink_subscription_registration_accounts(
                    origin,
                    reason.into(),
                    SubscriptionRegistrationOutcome::AddedBelowCapacity,
                );
            }
            AddAccountOutcome::Evicted(evicted) => {
                self.spawn_evicted_cleanup(evicted);
                inc_chainlink_subscription_registration_accounts(
                    origin,
                    reason.into(),
                    SubscriptionRegistrationOutcome::EvictedCandidate,
                );
            }
            AddAccountOutcome::NoEvictableCandidate => {
                self.cleanup_rejected_subscription(*pubkey).await?;
                inc_chainlink_subscription_registration_accounts(
                    origin,
                    reason.into(),
                    SubscriptionRegistrationOutcome::RejectedAndUnsubscribed,
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

    /// Moves a confirmed-missing account from the primary to the secondary
    /// tier. The tier move runs under one transition-lock scope; eviction
    /// cleanup is deferred to a detached task and the gRPC-only transport
    /// switch runs after the lock scope.
    /// Precondition: the caller holds the key's subscription guard.
    async fn move_not_found_to_secondary(&self, pubkey: Pubkey) {
        if self
            .capacity_eviction_protection_for(&pubkey)
            .is_protected()
        {
            return;
        }

        let direct_only = self
            .subscription_ownership
            .lock()
            .await
            .get(&pubkey)
            .is_some_and(|ownership| {
                ownership.reasons.len() == 1
                    && ownership.contains(SubscriptionReason::DirectAccount)
            });
        if !direct_only {
            return;
        }

        let (confirmed_missing, evicted) = {
            let _transition_guard =
                self.subscription_transition_lock.lock().await;

            if self.secondary.contains(&pubkey) {
                self.set_confirmed_missing(pubkey, true);
                (true, None)
            } else if !self.primary.contains(&pubkey) {
                (false, None)
            } else {
                match self.add_with_protection(&self.secondary, pubkey).await {
                    outcome @ (AddAccountOutcome::Added
                    | AddAccountOutcome::AlreadyPresent
                    | AddAccountOutcome::Evicted(_)) => {
                        self.primary.remove(&pubkey);
                        self.set_confirmed_missing(pubkey, true);
                        let evicted = match outcome {
                            AddAccountOutcome::Evicted(evicted) => {
                                Some(evicted)
                            }
                            _ => None,
                        };
                        (true, evicted)
                    }
                    AddAccountOutcome::NoEvictableCandidate => (false, None),
                }
            }
        };

        if let Some(evicted) = evicted {
            self.spawn_evicted_cleanup(evicted);
        }
        if confirmed_missing {
            // Drop the websocket leg promptly; the multiplexer only does so
            // after confirming gRPC coverage and errs otherwise, in which
            // case full coverage stays and the reconciler applies the
            // gRPC-only policy on a later pass.
            if let Err(err) =
                self.pubsub_client.prefer_grpc_subscription(pubkey).await
            {
                debug!(
                    pubkey = %pubkey,
                    error = ?err,
                    "Keeping full coverage for confirmed miss; reconciler applies gRPC-only policy later"
                );
            }
        }
    }

    /// Promotes a secondary-tier account that turned out to exist into the
    /// primary tier. The coverage-restoring subscribe runs before the state
    /// commit and outside the transition lock, so a subscribe failure leaves
    /// the tier state untouched.
    /// Precondition: the caller holds the key's subscription guard.
    async fn try_promote_found_to_primary(
        &self,
        pubkey: Pubkey,
        restore_full_coverage: bool,
    ) -> RemoteAccountProviderResult<PromotionOutcome> {
        // Not-in-secondary at entry is benign: the caller's key may hold
        // primary membership or never have been tiered (e.g. never-evict
        // keys). Only a mid-flight departure (re-check below) distinguishes
        // eviction.
        if !self.secondary.contains(&pubkey) {
            return Ok(PromotionOutcome::NotInSecondary);
        }

        if restore_full_coverage {
            self.pubsub_client.subscribe(pubkey, None).await?;
            self.set_confirmed_missing(pubkey, false);
        }

        let (outcome, evicted) = {
            let _transition_guard =
                self.subscription_transition_lock.lock().await;

            // Re-check under the lock: the key may have left the secondary
            // tier while the coverage subscribe was in flight — promoted by
            // another transition (benign) or evicted by another key's
            // admission (the found result must not count as admitted).
            if !self.secondary.contains(&pubkey) {
                (self.departed_promotion_outcome(&pubkey), None)
            } else {
                match self.add_with_protection(&self.primary, pubkey).await {
                    AddAccountOutcome::Added
                    | AddAccountOutcome::AlreadyPresent => {
                        self.secondary.remove(&pubkey);
                        self.set_confirmed_missing(pubkey, false);
                        (PromotionOutcome::Promoted, None)
                    }
                    AddAccountOutcome::Evicted(evicted) => {
                        self.secondary.remove(&pubkey);
                        self.set_confirmed_missing(pubkey, false);
                        (PromotionOutcome::Promoted, Some(evicted))
                    }
                    AddAccountOutcome::NoEvictableCandidate => {
                        (PromotionOutcome::NoCapacity, None)
                    }
                }
            }
        };

        if let Some(evicted) = evicted {
            self.spawn_evicted_cleanup(evicted);
        }
        Ok(outcome)
    }

    /// Outcome for a key that departed the secondary tier mid-promotion:
    /// primary membership means another transition promoted it; no
    /// membership means another key's admission evicted it.
    fn departed_promotion_outcome(&self, pubkey: &Pubkey) -> PromotionOutcome {
        if self.primary.contains(pubkey) {
            PromotionOutcome::NotInSecondary
        } else {
            PromotionOutcome::Evicted
        }
    }

    /// Admits a key whose pending fetch was just resolved as found by a
    /// subscription update before the fetch's subscription setup created any
    /// tier state: subscribes and registers it directly in the primary tier,
    /// so a found result is never handed to fetch waiters without primary
    /// admission. The in-flight setup adopts the membership (and skips its
    /// own subscribe). On rejection the caller fails the waiters; the
    /// placeholder ownership stays for the pending setup to adopt, which
    /// then registers the key as a fresh fetch-owned secondary entry.
    /// Precondition: the caller holds the key's subscription guard.
    async fn admit_resolved_fetch_to_primary(
        &self,
        pubkey: Pubkey,
    ) -> RemoteAccountProviderResult<()> {
        let has_capacity = {
            let _transition_guard =
                self.subscription_transition_lock.lock().await;
            self.has_capacity_with_protection(&self.primary, &pubkey)
                .await
        };
        if !has_capacity {
            return Err(
                RemoteAccountProviderError::NoEvictableSubscriptionCapacity {
                    pubkey,
                },
            );
        }

        self.pubsub_client.subscribe(pubkey, None).await?;

        let add_outcome = {
            let _transition_guard =
                self.subscription_transition_lock.lock().await;
            self.add_with_protection(&self.primary, pubkey).await
        };
        match add_outcome {
            AddAccountOutcome::Added | AddAccountOutcome::AlreadyPresent => {
                Ok(())
            }
            AddAccountOutcome::Evicted(evicted) => {
                self.spawn_evicted_cleanup(evicted);
                Ok(())
            }
            AddAccountOutcome::NoEvictableCandidate => {
                self.cleanup_rejected_subscription(pubkey).await?;
                Err(
                    RemoteAccountProviderError::NoEvictableSubscriptionCapacity {
                        pubkey,
                    },
                )
            }
        }
    }

    /// Drops the classification recorded for a pending-fetch winner whose
    /// primary admission failed: the rejection consumed the found evidence,
    /// and a later fetch must re-run the full tier classification instead of
    /// losing arbitration to it and returning the account from the secondary
    /// tier without primary admission.
    /// Precondition: the caller holds the key's subscription guard.
    async fn clear_rejected_fetch_classification(&self, pubkey: &Pubkey) {
        let _transition_guard = self.subscription_transition_lock.lock().await;
        let mut ownership = self.subscription_ownership.lock().await;
        if let Some(entry) = ownership.get_mut(pubkey) {
            if entry.is_empty() {
                ownership.remove(pubkey);
            } else {
                entry.last_classification = None;
            }
        }
    }
}

/// Result of trying to promote a secondary-tier account into the primary
/// tier. `NotInSecondary` (the key departed the secondary tier but holds
/// primary membership — another transition promoted it) is a benign no-op.
/// `Evicted` (the key departed with no membership — another key's admission
/// evicted it) means the found result must not count as admitted; the
/// detached eviction cleanup owns the state removal and bank eviction.
/// `NoCapacity` is a genuine capacity rejection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PromotionOutcome {
    Promoted,
    NoCapacity,
    NotInSecondary,
    Evicted,
}

pub(crate) enum SubscriptionReleaseMode {
    Single,
    All,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CapacityEvictionProtection {
    pub delegated: bool,
    pub undelegating: bool,
}

impl CapacityEvictionProtection {
    pub fn is_protected(self) -> bool {
        self.delegated || self.undelegating
    }
}

pub(crate) type CapacityEvictionProtectionPredicate =
    dyn Fn(&Pubkey) -> CapacityEvictionProtection + Send + Sync;
pub(crate) type SharedCapacityEvictionProtectionPredicate =
    Arc<RwLock<Option<Arc<CapacityEvictionProtectionPredicate>>>>;

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
    /// Serializes subscription transitions that can affect more than one
    /// pubkey. Acquiring one pubkey can evict and unsubscribe another pubkey
    /// from the LRU, so per-pubkey locks alone are not enough to keep
    /// ownership reasons and LRU membership in sync.
    subscription_transition_lock: Arc<AsyncMutex<()>>,
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
    lrucache_subscribed_accounts: Arc<AccountsLruCache>,

    /// Tracks fetch-owned accounts outside the primary working-set LRU.
    /// Pending fetches retain full coverage; confirmed misses prefer gRPC-only
    /// coverage until an account update promotes them to the primary tier.
    secondary_subscriptions: Arc<AccountsLruCache>,
    /// Bounded subset of the secondary tier proven missing by a winning RPC
    /// result. Reconciliation uses this to distinguish them from pending or
    /// failed fetches that must retain full transport coverage.
    confirmed_missing_subscriptions: Arc<Mutex<HashSet<Pubkey>>>,

    capacity_eviction_protection: SharedCapacityEvictionProtectionPredicate,

    /// Channel to notify when an account is removed from the cache and thus no
    /// longer being watched
    removed_account_tx: mpsc::Sender<Pubkey>,
    /// Single listener channel sending an update when an account is removed
    /// and no longer being watched.
    removed_account_rx: Mutex<Option<mpsc::Receiver<Pubkey>>>,

    subscription_forwarder: Arc<mpsc::Sender<ForwardedSubscriptionUpdate>>,

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
            context,
            kind,
            outcome,
            attempts as f64,
        );
        observe_chainlink_companion_fetch_duration_seconds(
            context,
            kind,
            outcome,
            started_at.elapsed().as_secs_f64(),
        );
    }
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

    /// Creates a background task that periodically reconciles subscriptions
    /// with the LRU (repairing missing ones, e.g. after a partial
    /// resubscription) and optionally updates the active subscriptions gauge
    #[allow(clippy::too_many_arguments)]
    fn start_active_subscriptions_updater<PubsubClient: ChainPubsubClient>(
        subscribed_accounts: Arc<AccountsLruCache>,
        secondary_subscriptions: Arc<AccountsLruCache>,
        confirmed_missing_subscriptions: Arc<Mutex<HashSet<Pubkey>>>,
        pubsub_client: Arc<PubsubClient>,
        removed_account_tx: mpsc::Sender<Pubkey>,
        subscription_key_locks: SubscriptionKeyLocks,
        subscription_ownership: SubscriptionOwnershipMap,
        fetching_accounts: Arc<FetchingAccounts>,
        capacity_eviction_protection: SharedCapacityEvictionProtectionPredicate,
        emit_metrics: bool,
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
                        &secondary_subscriptions,
                        &confirmed_missing_subscriptions,
                        pubsub_client.as_ref(),
                        &never_evicted,
                        &removed_account_tx,
                        Some(&subscription_key_locks),
                        Some(&subscription_ownership),
                        Some(fetching_accounts.as_ref()),
                        Some(&capacity_eviction_protection),
                    )
                    .await;

                debug!(count = pubsub_total, "Updating active subscriptions");
                if emit_metrics {
                    set_monitored_accounts_count(pubsub_total);
                }
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
        let secondary_subscriptions = Arc::new(AccountsLruCache::new(
            // SAFETY: config guarantees a non-zero capacity
            NonZeroUsize::new(config.secondary_subscriptions_lru_capacity())
                .expect("lru capacity must be non-zero"),
        ));
        Self::new_with_secondary_subscriptions(
            rpc_client,
            pubsub_client,
            subscription_forwarder,
            config,
            lrucache_subscribed_accounts,
            secondary_subscriptions,
            chain_slot,
        )
        .await
    }

    async fn new_with_secondary_subscriptions(
        rpc_client: T,
        pubsub_client: U,
        subscription_forwarder: mpsc::Sender<ForwardedSubscriptionUpdate>,
        config: &RemoteAccountProviderConfig,
        lrucache_subscribed_accounts: Arc<AccountsLruCache>,
        secondary_subscriptions: Arc<AccountsLruCache>,
        chain_slot: ChainSlot,
    ) -> RemoteAccountProviderResult<Self> {
        let (removed_account_tx, removed_account_rx) =
            tokio::sync::mpsc::channel(100);
        let subscription_key_locks: SubscriptionKeyLocks =
            Arc::new(AsyncMutex::new(HashMap::new()));
        let confirmed_missing_subscriptions =
            Arc::new(Mutex::new(HashSet::new()));
        let subscription_ownership: SubscriptionOwnershipMap =
            Arc::new(AsyncMutex::new(HashMap::new()));
        let fetching_accounts = Arc::<FetchingAccounts>::default();
        let capacity_eviction_protection:
            SharedCapacityEvictionProtectionPredicate =
            Arc::new(RwLock::new(None));

        // The reconciler always runs: partial resubscriptions rely on it for
        // repair. The config flag only gates the metrics emission.
        let active_subscriptions_updater =
            Some(Self::start_active_subscriptions_updater(
                lrucache_subscribed_accounts.clone(),
                secondary_subscriptions.clone(),
                confirmed_missing_subscriptions.clone(),
                Arc::new(pubsub_client.clone()),
                removed_account_tx.clone(),
                subscription_key_locks.clone(),
                subscription_ownership.clone(),
                fetching_accounts.clone(),
                capacity_eviction_protection.clone(),
                config.enable_subscription_metrics(),
            ));

        let me = Self {
            fetching_accounts,
            next_fetching_account_generation: AtomicU64::default(),
            subscription_ownership,
            subscription_transition_lock: Arc::new(AsyncMutex::new(())),
            subscription_key_locks,
            rpc_client,
            pubsub_client,
            chain_slot,
            last_update_slot: Arc::<AtomicU64>::default(),
            received_updates_count: Arc::<AtomicU64>::default(),
            lrucache_subscribed_accounts,
            secondary_subscriptions,
            confirmed_missing_subscriptions,
            capacity_eviction_protection,
            subscription_forwarder: Arc::new(subscription_forwarder),
            removed_account_tx,
            removed_account_rx: Mutex::new(Some(removed_account_rx)),
            _active_subscriptions_task_handle: active_subscriptions_updater,
        };

        let updates = me.pubsub_client.take_updates();
        me.listen_for_account_updates(updates)?;
        let clock_remote_account = me
            .try_get(
                clock::ID,
                AccountFetchContext::internal(AccountFetchReason::Clock),
            )
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
                        Arc::new(TieredSubscribedAccountsTracker::new(
                            provider.lrucache_subscribed_accounts.clone(),
                            provider.secondary_subscriptions.clone(),
                        )),
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
        let secondary_subscriptions = Arc::new(AccountsLruCache::new({
            let cap = config.secondary_subscriptions_lru_capacity();
            NonZeroUsize::new(cap).expect("non-zero capacity")
        }));
        let subscribed_accounts_tracker =
            Arc::new(TieredSubscribedAccountsTracker::new(
                subscribed_accounts.clone(),
                secondary_subscriptions.clone(),
            ));

        let submux =
            SubMuxClient::new(pubsubs, subscribed_accounts_tracker, None);

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
        >::new_with_secondary_subscriptions(
            rpc_client,
            submux,
            subscription_forwarder,
            config,
            subscribed_accounts,
            secondary_subscriptions,
            ChainSlot::new(chain_slot),
        )
        .await?;
        Ok((provider, deferred_pubsubs))
    }

    pub(crate) fn promote_accounts(&self, pubkeys: &[&Pubkey]) {
        self.lrucache_subscribed_accounts.promote_multi(pubkeys);
        // This runs on the per-transaction ensure path; the secondary tier
        // only holds fetch-owned/missing accounts and is empty in the common
        // case, so skip its lock entirely then. A promote missed due to a
        // concurrent insert is harmless (LRU ordering is a heuristic).
        if !self.secondary_subscriptions.is_vacant() {
            self.secondary_subscriptions.promote_multi(pubkeys);
        }
    }

    pub(crate) async fn get_slot(&self) -> RemoteAccountProviderResult<u64> {
        tokio::time::timeout(RPC_FETCH_TIMEOUT, self.rpc_client.get_slot())
            .await
            .map_err(|_| {
                RemoteAccountProviderError::AccountResolutionsFailed(format!(
                    "RPC call timeout fetching slot after {}ms",
                    RPC_FETCH_TIMEOUT.as_millis()
                ))
            })?
            .map_err(|err| {
                RemoteAccountProviderError::AccountResolutionsFailed(format!(
                    "RpcError fetching slot: {err:?}"
                ))
            })
    }

    pub(crate) async fn get_program_accounts_with_config(
        &self,
        pubkey: &Pubkey,
        mut config: RpcProgramAccountsConfig,
    ) -> RemoteAccountProviderResult<Vec<(Pubkey, Account)>> {
        config.account_config.commitment = Some(self.rpc_client.commitment());

        tokio::time::timeout(RPC_FETCH_TIMEOUT, async {
            self.rpc_client
                .get_program_accounts_with_config(pubkey, config)
                .await
        })
        .await
        .map_err(|_| {
            RemoteAccountProviderError::AccountResolutionsFailed(format!(
                "RPC call timeout fetching program accounts for {pubkey} after {}ms",
                RPC_FETCH_TIMEOUT.as_millis()
            ))
        })?
        .map_err(|err| {
            RemoteAccountProviderError::AccountResolutionsFailed(format!(
                "RpcError fetching program accounts for {pubkey}: {err:?}"
            ))
        })
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
        let subscription_tiers = Arc::new(self.subscription_tier_ctx());
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

                    let account_is_found = remote_account.is_found();

                    // Fast path: fetch arbitration and tier movement only
                    // apply while a fetch is pending or the account sits in
                    // the secondary tier. All other updates forward without
                    // taking the per-key guard or the transition lock.
                    let needs_tier_handling =
                        subscription_tiers.secondary.contains(&update.pubkey)
                            || fetching_accounts
                                .lock()
                                .unwrap_or_else(|poison| poison.into_inner())
                                .contains_key(&update.pubkey);

                    // Serialize fetch arbitration and tier movement so a late
                    // RPC result cannot overwrite this subscription update.
                    let mut classification_error = None;
                    let (forward_update, _accepted_update, resolved_fetch) =
                        if !needs_tier_handling {
                            // Record so a lagging RPC result cannot later win
                            // classification against this newer update.
                            if account_is_found {
                                subscription_tiers
                                    .record_classification(
                                        update.pubkey,
                                        slot,
                                        SubscriptionClassificationSource::Subscription,
                                    )
                                    .await;
                            }
                            (
                                Some(ForwardedSubscriptionUpdate {
                                    pubkey: update.pubkey,
                                    account: remote_account.clone(),
                                    source: update.source,
                                }),
                                true,
                                None,
                            )
                        } else {
                            // The per-key guard serializes this update
                            // against fetch resolutions and other transitions
                            // of the same key; the tier helpers scope the
                            // transition lock to their in-memory critical
                            // sections.
                            let _subscription_guard =
                                subscription_key_owned_guard_from_map(
                                    &subscription_tiers.subscription_key_locks,
                                    update.pubkey,
                                )
                                .await;
                            let classification_is_current = subscription_tiers
                                .classification_is_current(
                                    update.pubkey,
                                    slot,
                                    SubscriptionClassificationSource::Subscription,
                                )
                                .await;
                            let result = if classification_is_current {
                                let mut fetching =
                                    fetching_accounts.lock().unwrap_or_else(
                                        |poison| poison.into_inner(),
                                    );
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
                                        metrics::observe_chainlink_pending_fetch_owner_duration_seconds_with_context(
                                            state.fetch_context,
                                            ChainlinkPendingFetchLayer::RemoteAccountProvider,
                                            ChainlinkPendingFetchOutcome::ResolvedBySubscriptionUpdate,
                                            state.owner_started_at.elapsed().as_secs_f64(),
                                        );
                                        metrics::inc_chainlink_pending_fetch_accounts_with_context(
                                            state.fetch_context,
                                            ChainlinkPendingFetchLayer::RemoteAccountProvider,
                                            ChainlinkPendingFetchOutcome::ResolvedBySubscriptionUpdate,
                                            1,
                                        );

                                        (None, true, Some((generation, state.waiters)))
                                    } else {
                                        // Subscription is stale, put the fetch tracking back
                                        debug!(pubkey = %update.pubkey, slot = slot, fetch_start_slot = state.fetch_start_slot, generation, "Received stale subscription update");
                                        fetching.insert(update.pubkey, state);
                                        (None, false, None)
                                    }
                                } else {
                                    (None, false, None)
                                }
                                } else {
                                    (
                                        Some(ForwardedSubscriptionUpdate {
                                            pubkey: update.pubkey,
                                            account: remote_account.clone(),
                                            source: update.source,
                                        }),
                                        true,
                                        None,
                                    )
                                }
                            } else {
                                debug!(pubkey = %update.pubkey, slot, "Ignoring stale subscription classification");
                                (None, false, None)
                            };

                            // The in-flight acquisition may not have created
                            // the ownership entry yet; record so the later
                            // RPC result loses arbitration.
                            let apply_classification = result.1
                                && account_is_found
                                && match result.2.as_ref() {
                                    Some((generation, _)) => {
                                        subscription_tiers
                                            .record_classification_for_pending_fetch(
                                                update.pubkey,
                                                slot,
                                                SubscriptionClassificationSource::Subscription,
                                                *generation,
                                            )
                                            .await
                                    }
                                    None => {
                                        subscription_tiers
                                            .record_classification(
                                                update.pubkey,
                                                slot,
                                                SubscriptionClassificationSource::Subscription,
                                            )
                                            .await
                                    }
                                };
                            if apply_classification
                                && !subscription_tiers
                                    .secondary
                                    .contains(&update.pubkey)
                                && result.2.is_some()
                                && !subscription_tiers
                                    .primary
                                    .contains(&update.pubkey)
                            {
                                // The pending fetch resolved before its
                                // subscription setup created any tier state;
                                // admit the found account into the primary
                                // tier now so it is never handed to waiters
                                // without primary admission.
                                if let Err(err) = subscription_tiers
                                    .admit_resolved_fetch_to_primary(
                                        update.pubkey,
                                    )
                                    .await
                                {
                                    warn!(pubkey = %update.pubkey, error = ?err, "Failed to admit resolved-fetch account to primary subscription tier");
                                    subscription_tiers
                                        .clear_rejected_fetch_classification(
                                            &update.pubkey,
                                        )
                                        .await;
                                    classification_error =
                                        Some(err.to_string());
                                }
                            } else if apply_classification
                                && subscription_tiers
                                    .secondary
                                    .contains(&update.pubkey)
                            {
                                match subscription_tiers
                                    .try_promote_found_to_primary(
                                        update.pubkey,
                                        true,
                                    )
                                    .await
                                {
                                    Ok(PromotionOutcome::Promoted) => {}
                                    // The key was promoted by another
                                    // transition while this one was in
                                    // flight; it holds primary membership.
                                    Ok(PromotionOutcome::NotInSecondary) => {}
                                    // Evicted mid-promotion: the detached
                                    // eviction cleanup owns the follow-up;
                                    // the found update must not be forwarded
                                    // without primary membership.
                                    Ok(PromotionOutcome::Evicted) => {
                                        classification_error = Some(
                                            RemoteAccountProviderError::NoEvictableSubscriptionCapacity {
                                                pubkey: update.pubkey,
                                            }
                                            .to_string(),
                                        );
                                    }
                                    Ok(PromotionOutcome::NoCapacity) => {
                                        let err = RemoteAccountProviderError::NoEvictableSubscriptionCapacity {
                                            pubkey: update.pubkey,
                                        };
                                        subscription_tiers
                                            .finalize_rejected_promotion(
                                                &update.pubkey,
                                            )
                                            .await;
                                        classification_error =
                                            Some(err.to_string());
                                    }
                                    Err(err) => {
                                        warn!(pubkey = %update.pubkey, error = ?err, "Failed to promote found account to primary subscription tier");
                                        classification_error =
                                            Some(err.to_string());
                                    }
                                }
                            }
                            result
                        };

                    if let Some((_, waiters)) = resolved_fetch {
                        for sender in waiters {
                            let response = match classification_error.as_ref() {
                                Some(err) => Err(RemoteAccountProviderError::AccountResolutionsFailed(err.clone())),
                                None => Ok(remote_account.clone()),
                            };
                            let _ = sender.send(response);
                        }
                    }

                    if let Some(forward_update) = forward_update
                        .filter(|_| classification_error.is_none())
                    {
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
    #[instrument(skip(self, fetch_context))]
    pub async fn try_get(
        &self,
        pubkey: Pubkey,
        fetch_context: impl Into<AccountFetchContext>,
    ) -> RemoteAccountProviderResult<RemoteAccount> {
        self.try_get_multi(&[pubkey], None, fetch_context, None)
            .await
            // SAFETY: we are guaranteed to have a single result here as
            // otherwise we would have gotten an error
            .map(|mut accs| accs.drain(..).next().unwrap())
    }

    #[instrument(skip(self, pubkeys, config, fetch_context))]
    pub async fn try_get_multi_until_slots_match(
        &self,
        pubkeys: &[Pubkey],
        config: Option<MatchSlotsConfig>,
        fetch_context: impl Into<AccountFetchContext>,
    ) -> RemoteAccountProviderResult<Vec<RemoteAccount>> {
        use SlotsMatchResult::*;
        let fetch_context = fetch_context.into();
        let companion_fetch_kind =
            config.as_ref().map(|config| config.companion_fetch_kind);
        let config = config
            .as_ref()
            .map(MatchSlotsRetryConfig::from)
            .unwrap_or_default();
        let companion_fetch_started_at = std::time::Instant::now();
        let mut companion_fetch_attempts = 1u64;
        // 1. Fetch the _normal_ way and hope the slots match and if required
        //    the min_context_slot is met
        let mut remote_accounts = match self
            .try_get_multi(pubkeys, None, fetch_context, None)
            .await
        {
            Ok(accounts) => accounts,
            Err(err) => {
                observe_companion_fetch_if_configured(
                    fetch_context,
                    companion_fetch_kind,
                    ChainlinkCompanionFetchOutcome::FailedRpc,
                    companion_fetch_attempts,
                    companion_fetch_started_at,
                );
                return Err(err);
            }
        };
        if let Match = slots_match_and_meet_min_context(
            &remote_accounts,
            config.min_context_slot,
        ) {
            observe_companion_fetch_if_configured(
                fetch_context,
                companion_fetch_kind,
                ChainlinkCompanionFetchOutcome::Succeeded,
                companion_fetch_attempts,
                companion_fetch_started_at,
            );
            return Ok(remote_accounts);
        }

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
            companion_fetch_attempts += 1;
            remote_accounts = match self
                .fetch_multi_rpc_only(pubkeys, fetch_start_slot, fetch_context)
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
                        Err(_) => {
                            observe_companion_fetch_if_configured(
                                fetch_context,
                                companion_fetch_kind,
                                ChainlinkCompanionFetchOutcome::FailedRpc,
                                companion_fetch_attempts,
                                companion_fetch_started_at,
                            );
                            return Err(err);
                        }
                    }
                }
            };
            for (pubkey, remote_account) in pubkeys.iter().zip(&remote_accounts)
            {
                let _subscription_guard =
                    subscription_key_owned_guard_from_map(
                        &self.subscription_key_locks,
                        *pubkey,
                    )
                    .await;
                self.subscription_tier_ctx()
                    .apply_fetch_classification(
                        pubkey,
                        remote_account.slot(),
                        !remote_account.is_found(),
                    )
                    .await?;
            }
            let slots_match_result = slots_match_and_meet_min_context(
                &remote_accounts,
                config.min_context_slot,
            );
            if let Match = slots_match_result {
                observe_companion_fetch_if_configured(
                    fetch_context,
                    companion_fetch_kind,
                    ChainlinkCompanionFetchOutcome::Succeeded,
                    companion_fetch_attempts,
                    companion_fetch_started_at,
                );
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
                            observe_companion_fetch_if_configured(
                                fetch_context,
                                companion_fetch_kind,
                                ChainlinkCompanionFetchOutcome::FailedSlotMismatch,
                                companion_fetch_attempts,
                                companion_fetch_started_at,
                            );
                            return Err(
                                RemoteAccountProviderError::SlotsDidNotMatch(
                                    pubkeys_str(pubkeys),
                                    remote_account_slots,
                                    limit,
                                ),
                            );
                        }
                        MatchButBelowMinContextSlot(slot) => {
                            observe_companion_fetch_if_configured(
                                fetch_context,
                                companion_fetch_kind,
                                ChainlinkCompanionFetchOutcome::FailedMinContextSlot,
                                companion_fetch_attempts,
                                companion_fetch_started_at,
                            );
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
    #[instrument(skip(self, pubkeys, mark_empty_if_not_found, fetch_context))]
    pub async fn try_get_multi(
        &self,
        pubkeys: &[Pubkey],
        mark_empty_if_not_found: Option<&[Pubkey]>,
        fetch_context: impl Into<AccountFetchContext>,
        fetch_start_slot: Option<u64>,
    ) -> RemoteAccountProviderResult<Vec<RemoteAccount>> {
        if pubkeys.is_empty() {
            return Ok(vec![]);
        }
        let fetch_context = fetch_context.into();

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
        let mut await_receivers: Vec<(
            Pubkey,
            oneshot::Receiver<FetchResult>,
            PendingFetchWaiterGaugeGuard,
        )> = Vec::with_capacity(pubkeys.len());

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
            let mut fetching = self
                .fetching_accounts
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            for &pubkey in pubkeys {
                let (sender, receiver) = oneshot::channel();
                let mut claimed = false;
                let layer = ChainlinkPendingFetchLayer::RemoteAccountProvider;
                let mut waiter_guard =
                    PendingFetchWaiterGaugeGuard::inactive(layer);
                match fetching.entry(pubkey) {
                    Entry::Occupied(mut entry) => {
                        entry.get_mut().waiters.push(sender);
                        inc_chainlink_pending_fetch_accounts_with_context(
                            fetch_context,
                            layer,
                            ChainlinkPendingFetchOutcome::JoinedExisting,
                            1,
                        );
                        inc_chainlink_pending_fetch_waiters_with_context(
                            fetch_context,
                            layer,
                            1,
                        );
                        inc_chainlink_pending_fetch_waiters_gauge(layer);
                        waiter_guard =
                            PendingFetchWaiterGaugeGuard::active(layer);
                    }
                    Entry::Vacant(entry) => {
                        let generation =
                            self.next_fetching_account_generation();
                        entry.insert(FetchingAccountState {
                            generation,
                            fetch_start_slot,
                            fetch_context,
                            owner_started_at: std::time::Instant::now(),
                            waiters: vec![sender],
                        });
                        inc_chainlink_pending_fetch_accounts_with_context(
                            fetch_context,
                            layer,
                            ChainlinkPendingFetchOutcome::Owned,
                            1,
                        );
                        claimed_generations.insert(pubkey, generation);
                        claimed = true;
                    }
                }
                if claimed {
                    claimed_pubkeys.push(pubkey);
                }
                await_receivers.push((pubkey, receiver, waiter_guard));
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
                    self.subscription_ownership.clone(),
                    self.subscription_transition_lock.clone(),
                    self.lrucache_subscribed_accounts.clone(),
                    self.secondary_subscriptions.clone(),
                    claimed_pubkeys.clone(),
                    claimed_generations.clone(),
                );
            if let Err(err) = self
                .setup_subscriptions(&claimed_pubkeys, fetch_context)
                .await
            {
                subscription_setup_guard
                    .cleanup_with_error(err.to_string())
                    .await;
                return Err(err);
            }
            subscription_setup_guard.disarm();

            // Start the fetch for the claimed pubkeys only. Claim sets
            // above the RPC limit are split into evenly sized chunks
            let min_context_slot = fetch_start_slot;
            for chunk in balanced_chunks(claimed_pubkeys) {
                self.fetch(
                    chunk,
                    claimed_generations.clone(),
                    mark_empty_if_not_found,
                    min_context_slot,
                    fetch_context,
                );
            }
        }

        // Wait for all accounts to resolve (either from fetch or
        // subscription override). We await receivers in input pubkey
        // order so the returned Vec is index-aligned with `pubkeys`.
        let mut resolved_accounts = vec![];
        let mut errors = vec![];

        for (idx, (pubkey, receiver, mut waiter_guard)) in
            await_receivers.into_iter().enumerate()
        {
            let receiver_result = receiver.await;
            waiter_guard.finish();
            match receiver_result {
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
        fetch_context: AccountFetchContext,
    ) -> RemoteAccountProviderResult<Vec<RemoteAccount>> {
        // This must stay a single wire call so all results share one
        // response slot (the slot-match contract callers verify);
        // slot-consistent sets must fit within the RPC limit.
        debug_assert!(
            pubkeys.len() <= MAX_MULTIPLE_ACCOUNTS_PER_REQUEST,
            "fetch_multi_rpc_only cannot chunk {} keys (limit {})",
            pubkeys.len(),
            MAX_MULTIPLE_ACCOUNTS_PER_REQUEST
        );
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
        inc_account_fetches_found_with_context(fetch_context, found_count);
        inc_account_fetches_not_found_with_context(
            fetch_context,
            not_found_count,
        );

        Ok(remote_accounts)
    }

    async fn setup_subscriptions(
        &self,
        pubkeys: &[Pubkey],
        fetch_context: AccountFetchContext,
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
            join_all(pubkeys.iter().map(|pubkey| async move {
                self.acquire_subscription_with_origin(
                    pubkey,
                    SubscriptionReason::DirectAccount,
                    SubscriptionRegistrationOrigin::Fetch(fetch_context),
                )
                .await
            }))
            .await;

        let mut errors = Vec::new();
        let mut acquired = Vec::new();
        for (result, pubkey) in
            subscription_results.into_iter().zip(pubkeys.iter())
        {
            match result {
                Err(err) => {
                    error!(
                        pubkey = %pubkey, err = ?err,
                        "Failed to subscribe to account"
                    );
                    errors.push((*pubkey, err));
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
            // A single failure keeps its type so callers can react to
            // specific variants (e.g. capacity exhaustion).
            if errors.len() == 1 {
                // SAFETY: len checked above
                return Err(errors.pop().unwrap().1);
            }
            return Err(
                RemoteAccountProviderError::AccountSubscriptionsTaskFailed(
                    format!(
                        "{} subscription(s) failed: [{}]",
                        errors.len(),
                        errors
                            .iter()
                            .map(|(pubkey, err)| format!("{pubkey}: {err}"))
                            .collect::<Vec<_>>()
                            .join(", ")
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
        origin: SubscriptionRegistrationOrigin,
    ) -> RemoteAccountProviderResult<()> {
        if matches!(origin, SubscriptionRegistrationOrigin::Fetch(_))
            && reason == SubscriptionReason::DirectAccount
            && self.lrucache_subscribed_accounts.can_evict(pubkey)
        {
            return self
                .subscription_tier_ctx()
                .register_secondary(pubkey, reason, origin)
                .await;
        }

        let tier_ctx = self.subscription_tier_ctx();
        let has_capacity = {
            let _transition_guard =
                self.subscription_transition_lock.lock().await;
            tier_ctx
                .has_capacity_with_protection(
                    &self.lrucache_subscribed_accounts,
                    pubkey,
                )
                .await
        };
        if !has_capacity {
            inc_chainlink_subscription_registration_accounts(
                origin,
                reason.into(),
                SubscriptionRegistrationOutcome::RejectedNoCapacity,
            );
            return Err(
                RemoteAccountProviderError::NoEvictableSubscriptionCapacity {
                    pubkey: *pubkey,
                },
            );
        }

        // 1. First realize subscription. Runs outside the transition lock;
        // the per-key guard held by the caller serializes this key.
        if let Err(err) = self.pubsub_client.subscribe(*pubkey, None).await {
            inc_chainlink_subscription_registration_accounts(
                origin,
                reason.into(),
                SubscriptionRegistrationOutcome::SubscribeError,
            );
            return Err(err);
        }

        // 2. Add to LRU cache
        // If an account is evicted then we need to unsubscribe from it
        // and then inform upstream that we are no longer tracking it
        let add_outcome = {
            let _transition_guard =
                self.subscription_transition_lock.lock().await;
            let add_outcome = tier_ctx
                .add_with_protection(
                    &self.lrucache_subscribed_accounts,
                    *pubkey,
                )
                .await;
            if !matches!(add_outcome, AddAccountOutcome::NoEvictableCandidate) {
                self.remove_from_secondary(pubkey);
            }
            add_outcome
        };

        match add_outcome {
            AddAccountOutcome::AlreadyPresent => {
                inc_chainlink_subscription_registration_accounts(
                    origin,
                    reason.into(),
                    SubscriptionRegistrationOutcome::AlreadyPresent,
                );
            }
            AddAccountOutcome::Added => {
                inc_chainlink_subscription_registration_accounts(
                    origin,
                    reason.into(),
                    SubscriptionRegistrationOutcome::AddedBelowCapacity,
                );
            }
            AddAccountOutcome::Evicted(evicted) => {
                trace!(evicted = %evicted, "Evicting account");
                tier_ctx.spawn_evicted_cleanup(evicted);
                inc_chainlink_subscription_registration_accounts(
                    origin,
                    reason.into(),
                    SubscriptionRegistrationOutcome::EvictedCandidate,
                );
            }
            AddAccountOutcome::NoEvictableCandidate => {
                tier_ctx.cleanup_rejected_subscription(*pubkey).await?;
                debug!(
                    pubkey = %pubkey,
                    "No evictable subscription capacity available; all LRU candidates are protected"
                );
                inc_chainlink_subscription_registration_accounts(
                    origin,
                    reason.into(),
                    SubscriptionRegistrationOutcome::RejectedAndUnsubscribed,
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
            || self.secondary_subscriptions.contains(pubkey)
    }

    /// Removes a pubkey from the secondary LRU; safe for never-evict keys.
    fn remove_from_secondary(&self, pubkey: &Pubkey) {
        if self.secondary_subscriptions.contains(pubkey) {
            self.secondary_subscriptions.remove(pubkey);
        }
        self.confirmed_missing_subscriptions
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .remove(pubkey);
    }

    fn subscription_tier_ctx(&self) -> SubscriptionTierCtx<U> {
        SubscriptionTierCtx {
            primary: self.lrucache_subscribed_accounts.clone(),
            secondary: self.secondary_subscriptions.clone(),
            pubsub_client: self.pubsub_client.clone(),
            subscription_ownership: self.subscription_ownership.clone(),
            subscription_transition_lock: self
                .subscription_transition_lock
                .clone(),
            subscription_key_locks: self.subscription_key_locks.clone(),
            fetching_accounts: self.fetching_accounts.clone(),
            capacity_eviction_protection: self
                .capacity_eviction_protection
                .clone(),
            confirmed_missing_subscriptions: self
                .confirmed_missing_subscriptions
                .clone(),
            removed_account_tx: self.removed_account_tx.clone(),
        }
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

    async fn subscription_key_lock(
        &self,
        pubkey: &Pubkey,
    ) -> Arc<AsyncMutex<()>> {
        subscription_key_lock_from_map(&self.subscription_key_locks, pubkey)
            .await
    }

    pub async fn acquire_subscription(
        &self,
        pubkey: &Pubkey,
        reason: SubscriptionReason,
    ) -> RemoteAccountProviderResult<()> {
        self.acquire_subscription_with_mode(
            pubkey,
            reason,
            false,
            SubscriptionRegistrationOrigin::Internal,
        )
        .await
    }

    async fn acquire_subscription_with_origin(
        &self,
        pubkey: &Pubkey,
        reason: SubscriptionReason,
        origin: SubscriptionRegistrationOrigin,
    ) -> RemoteAccountProviderResult<()> {
        self.acquire_subscription_with_mode(pubkey, reason, false, origin)
            .await
    }

    pub async fn ensure_subscription(
        &self,
        pubkey: &Pubkey,
        reason: SubscriptionReason,
    ) -> RemoteAccountProviderResult<()> {
        self.acquire_subscription_with_mode(
            pubkey,
            reason,
            true,
            SubscriptionRegistrationOrigin::Internal,
        )
        .await
    }

    async fn acquire_subscription_with_mode(
        &self,
        pubkey: &Pubkey,
        reason: SubscriptionReason,
        skip_existing_reason: bool,
        origin: SubscriptionRegistrationOrigin,
    ) -> RemoteAccountProviderResult<()> {
        // The per-key guard serializes every transition of this key,
        // including the network calls made below. The transition lock is
        // acquired only inside the tier-state helpers.
        let subscription_key_lock = self.subscription_key_lock(pubkey).await;
        let _subscription_guard = subscription_key_lock.lock().await;

        let mut ownership = self.subscription_ownership.lock().await;
        if let Some(existing) = ownership.get_mut(pubkey) {
            let classification_placeholder_generation =
                existing.classification_placeholder_generation;
            let acquired_reason =
                !skip_existing_reason || !existing.contains(reason);
            if acquired_reason {
                existing.acquire(reason);
            }
            drop(ownership);

            let repair_result = if self
                .lrucache_subscribed_accounts
                .contains(pubkey)
            {
                self.lrucache_subscribed_accounts.promote_multi(&[pubkey]);
                Ok(())
            } else if self.secondary_subscriptions.contains(pubkey) {
                self.secondary_subscriptions.promote_multi(&[pubkey]);
                let keep_secondary =
                    matches!(origin, SubscriptionRegistrationOrigin::Fetch(_))
                        && reason == SubscriptionReason::DirectAccount;
                if !keep_secondary {
                    match self
                        .subscription_tier_ctx()
                        .try_promote_found_to_primary(*pubkey, true)
                        .await
                    {
                        Ok(PromotionOutcome::Promoted) => Ok(()),
                        // Promoted by another transition mid-flight; the key
                        // holds primary membership and the reason stands.
                        Ok(PromotionOutcome::NotInSecondary) => Ok(()),
                        // Evicted by another key's admission mid-flight;
                        // register it from scratch.
                        Ok(PromotionOutcome::Evicted) => {
                            self.register_subscription(pubkey, reason, origin)
                                .await
                        }
                        Ok(PromotionOutcome::NoCapacity)
                            if reason
                                == SubscriptionReason::UndelegationTracking =>
                        {
                            Ok(())
                        }
                        Ok(PromotionOutcome::NoCapacity) => Err(
                            RemoteAccountProviderError::NoEvictableSubscriptionCapacity {
                                pubkey: *pubkey,
                            },
                        ),
                        Err(err) => Err(err),
                    }
                } else {
                    let confirmed_missing = self
                        .confirmed_missing_subscriptions
                        .lock()
                        .unwrap_or_else(|poison| poison.into_inner())
                        .contains(pubkey);
                    if confirmed_missing {
                        // Flow the error into repair_result (no early return)
                        // so the acquired-reason rollback below executes.
                        match self.pubsub_client.subscribe(*pubkey, None).await
                        {
                            Ok(()) => {
                                self.confirmed_missing_subscriptions
                                    .lock()
                                    .unwrap_or_else(|poison| {
                                        poison.into_inner()
                                    })
                                    .remove(pubkey);
                                Ok(())
                            }
                            Err(err) => Err(err),
                        }
                    } else {
                        Ok(())
                    }
                }
            } else {
                self.register_subscription(pubkey, reason, origin).await
            };

            if let Err(err) = repair_result {
                if acquired_reason {
                    if let Some(existing) =
                        self.subscription_ownership.lock().await.get_mut(pubkey)
                    {
                        if existing.release(reason) {
                            existing.classification_placeholder_generation =
                                classification_placeholder_generation;
                        }
                    }
                }
                return Err(err);
            }
            inc_chainlink_subscription_registration_accounts(
                origin,
                reason.into(),
                SubscriptionRegistrationOutcome::AlreadyPresent,
            );
            return Ok(());
        }
        drop(ownership);

        self.register_subscription(pubkey, reason, origin).await?;

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
        let subscription_key_lock = self.subscription_key_lock(pubkey).await;
        let _subscription_guard = subscription_key_lock.lock().await;

        if !self.lrucache_subscribed_accounts.can_evict(pubkey) {
            inc_chainlink_subscription_release_accounts(
                reason.into(),
                SubscriptionReleaseOutcome::RetainedIntentionally,
            );
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
                None => {
                    inc_chainlink_subscription_release_accounts(
                        reason.into(),
                        SubscriptionReleaseOutcome::AlreadyAbsent,
                    );
                    return Ok(false);
                }
            };
            if !is_empty {
                inc_chainlink_subscription_release_accounts(
                    reason.into(),
                    SubscriptionReleaseOutcome::RetainedOtherReasons,
                );
                return Ok(false);
            }
            released_count
        };

        let success = subscription_reconciler::unsubscribe_and_notify_removal(
            *pubkey,
            &self.pubsub_client,
            &self.removed_account_tx,
            SubscriptionCleanupSource::NormalRelease,
        )
        .await;

        if success {
            inc_chainlink_subscription_release_accounts(
                reason.into(),
                SubscriptionReleaseOutcome::Unsubscribed,
            );
            let _transition_guard =
                self.subscription_transition_lock.lock().await;
            self.subscription_ownership.lock().await.remove(pubkey);
            self.lrucache_subscribed_accounts.remove(pubkey);
            self.remove_from_secondary(pubkey);
        } else {
            inc_chainlink_subscription_release_accounts(
                reason.into(),
                SubscriptionReleaseOutcome::UnsubscribeFailed,
            );
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
                None => {
                    inc_chainlink_subscription_release_accounts(
                        reason.into(),
                        SubscriptionReleaseOutcome::AlreadyAbsent,
                    );
                    inc_chainlink_subscription_cleanup_accounts(
                        SubscriptionCleanupSource::DelegatedAccountSilent,
                        SubscriptionCleanupOutcome::AlreadyAbsent,
                    );
                    return Ok(false);
                }
            };

            if released_count == 0 {
                inc_chainlink_subscription_release_accounts(
                    reason.into(),
                    SubscriptionReleaseOutcome::AlreadyAbsent,
                );
                inc_chainlink_subscription_cleanup_accounts(
                    SubscriptionCleanupSource::DelegatedAccountSilent,
                    SubscriptionCleanupOutcome::AlreadyAbsent,
                );
                return Ok(false);
            }

            if !is_empty {
                inc_chainlink_subscription_release_accounts(
                    reason.into(),
                    SubscriptionReleaseOutcome::RetainedOtherReasons,
                );
                inc_chainlink_subscription_cleanup_accounts(
                    SubscriptionCleanupSource::DelegatedAccountSilent,
                    SubscriptionCleanupOutcome::RetainedIntentionally,
                );
                trace!(
                    pubkey = %pubkey,
                    ?reason,
                    released_count,
                    "Released delegated-account subscription ownership; \
                     kept protected/live subscription and LRU entry"
                );
                return Ok(false);
            }

            released_count
        };

        match self.pubsub_client.unsubscribe(*pubkey).await {
            Ok(()) => {
                inc_chainlink_subscription_release_accounts(
                    reason.into(),
                    SubscriptionReleaseOutcome::Unsubscribed,
                );
                inc_chainlink_subscription_cleanup_accounts(
                    SubscriptionCleanupSource::DelegatedAccountSilent,
                    SubscriptionCleanupOutcome::Unsubscribed,
                );
                {
                    let _transition_guard =
                        self.subscription_transition_lock.lock().await;
                    self.subscription_ownership.lock().await.remove(pubkey);
                    self.lrucache_subscribed_accounts.remove(pubkey);
                    self.remove_from_secondary(pubkey);
                }
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
                    inc_chainlink_subscription_release_accounts(
                        reason.into(),
                        SubscriptionReleaseOutcome::AlreadyAbsent,
                    );
                    inc_chainlink_subscription_cleanup_accounts(
                        SubscriptionCleanupSource::DelegatedAccountSilent,
                        SubscriptionCleanupOutcome::AlreadyAbsent,
                    );
                    {
                        let _transition_guard =
                            self.subscription_transition_lock.lock().await;
                        self.subscription_ownership.lock().await.remove(pubkey);
                        self.lrucache_subscribed_accounts.remove(pubkey);
                        self.remove_from_secondary(pubkey);
                    }
                    trace!(
                        pubkey = %pubkey,
                        ?reason,
                        "Removed stale delegated-account LRU entry for missing subscription"
                    );
                    return Ok(false);
                }

                inc_chainlink_subscription_release_accounts(
                    reason.into(),
                    SubscriptionReleaseOutcome::UnsubscribeFailed,
                );
                inc_chainlink_subscription_cleanup_accounts(
                    SubscriptionCleanupSource::DelegatedAccountSilent,
                    SubscriptionCleanupOutcome::UnsubscribeFailed,
                );
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

    /// Unsubscribe from an account
    #[instrument(skip(self))]
    pub async fn unsubscribe(
        &self,
        pubkey: &Pubkey,
    ) -> RemoteAccountProviderResult<()> {
        let subscription_key_lock = self.subscription_key_lock(pubkey).await;
        let _subscription_guard = subscription_key_lock.lock().await;

        if !self.lrucache_subscribed_accounts.can_evict(pubkey) {
            warn!(pubkey = %pubkey, "Tried to unsubscribe from account that should never be evicted");
            inc_chainlink_subscription_cleanup_accounts(
                SubscriptionCleanupSource::ManualUnsubscribe,
                SubscriptionCleanupOutcome::RetainedIntentionally,
            );
            return Ok(());
        }

        if !self.lrucache_subscribed_accounts.contains(pubkey)
            && !self.secondary_subscriptions.contains(pubkey)
        {
            trace!(pubkey = %pubkey, "Already unsubscribed from LRU");
            inc_chainlink_subscription_cleanup_accounts(
                SubscriptionCleanupSource::ManualUnsubscribe,
                SubscriptionCleanupOutcome::AlreadyAbsent,
            );
            return Ok(());
        }

        let success = subscription_reconciler::unsubscribe_and_notify_removal(
            *pubkey,
            &self.pubsub_client,
            &self.removed_account_tx,
            SubscriptionCleanupSource::ManualUnsubscribe,
        )
        .await;

        if success {
            let _transition_guard =
                self.subscription_transition_lock.lock().await;
            self.lrucache_subscribed_accounts.remove(pubkey);
            self.remove_from_secondary(pubkey);
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
        fetch_context: AccountFetchContext,
    ) {
        let rpc_client = self.rpc_client.clone();
        let fetching_accounts = self.fetching_accounts.clone();
        let commitment = self.rpc_client.commitment();
        let mark_empty_if_not_found =
            mark_empty_if_not_found.unwrap_or(&[]).to_vec();
        let subscription_tiers = self.subscription_tier_ctx();
        tokio::spawn(async move {
            use RemoteAccount::*;

            let fetch_started_at = std::time::Instant::now();
            // Helper to notify all pending requests of fetch failure
            let notify_error = |error_msg: &str| {
                let mut fetching = fetching_accounts
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner());
                warn!(
                    pubkey_count = pubkeys.len(),
                    pubkeys = %pubkeys_str(&pubkeys),
                    min_context_slot = min_context_slot,
                    commitment = ?commitment,
                    fetch_entrypoint = %fetch_context.entrypoint(),
                    fetch_reason = %fetch_context.reason(),
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
                            observe_chainlink_pending_fetch_owner_duration_seconds_with_context(
                                state.fetch_context,
                                ChainlinkPendingFetchLayer::RemoteAccountProvider,
                                ChainlinkPendingFetchOutcome::OwnerFailed,
                                state.owner_started_at.elapsed().as_secs_f64(),
                            );
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
                                fetch_entrypoint = %fetch_context.entrypoint(),
                        fetch_reason = %fetch_context.reason(),
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
            let mut not_found_pubkeys = HashSet::new();

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
                        not_found_pubkeys.insert(*pubkey);
                        inc_chainlink_empty_placeholder_accounts_total_with_context(
                            fetch_context,
                            ChainlinkEmptyPlaceholderStage::ConvertedToEmpty,
                            Outcome::Success,
                        );
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
                        not_found_pubkeys.insert(*pubkey);
                        NotFound(response_slot)
                    }
                })
                .collect();

            // Update metrics for successful RPC fetch
            inc_account_fetches_success(pubkeys.len() as u64);
            inc_account_fetches_found_with_context(fetch_context, found_count);
            inc_account_fetches_not_found_with_context(
                fetch_context,
                not_found_count,
            );

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
                let (waiters, classification_result) = {
                    // The per-key guard serializes this resolution against
                    // subscription updates and other transitions of the same
                    // key; the tier helpers scope the transition lock to
                    // their in-memory critical sections.
                    let _subscription_guard =
                        subscription_key_owned_guard_from_map(
                            &subscription_tiers.subscription_key_locks,
                            *pubkey,
                        )
                        .await;
                    // Remove from fetching and get pending requests
                    // Note: the account might have been resolved by a
                    // subscription update already or replaced by a newer owner.
                    let Some(generation) = generations.get(pubkey).copied()
                    else {
                        continue;
                    };
                    let state = {
                        let mut fetching = fetching_accounts
                            .lock()
                            .unwrap_or_else(|poison| poison.into_inner());
                        remove_fetching_account_if_generation_matches(
                            &mut fetching,
                            pubkey,
                            generation,
                        )
                    };
                    if let Some(state) = state {
                        let waiters = state.waiters;

                        let classification_result = subscription_tiers
                            .apply_fetch_classification(
                                pubkey,
                                response_slot,
                                not_found_pubkeys.contains(pubkey),
                            )
                            .await;
                        observe_chainlink_pending_fetch_owner_duration_seconds_with_context(
                            state.fetch_context,
                            ChainlinkPendingFetchLayer::RemoteAccountProvider,
                            if classification_result.is_ok() {
                                ChainlinkPendingFetchOutcome::OwnerSucceeded
                            } else {
                                ChainlinkPendingFetchOutcome::OwnerFailed
                            },
                            state.owner_started_at.elapsed().as_secs_f64(),
                        );
                        (waiters, classification_result)
                    } else {
                        inc_chainlink_pending_fetch_accounts_with_context(
                            fetch_context,
                            ChainlinkPendingFetchLayer::RemoteAccountProvider,
                            ChainlinkPendingFetchOutcome::RpcFetchCompletedAfterUpdate,
                            1,
                        );
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
                    let result = match &classification_result {
                        Ok(()) => Ok(remote_account.clone()),
                        Err(RemoteAccountProviderError::NoEvictableSubscriptionCapacity { pubkey }) => {
                            Err(RemoteAccountProviderError::NoEvictableSubscriptionCapacity {
                                pubkey: *pubkey,
                            })
                        }
                        Err(err) => Err(
                            RemoteAccountProviderError::AccountResolutionsFailed(
                                err.to_string(),
                            ),
                        ),
                    };
                    let _ = request.send(result);
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

#[cfg(any(test, feature = "dev-context"))]
impl<T: ChainRpcClient, U: ChainPubsubClient> RemoteAccountProvider<T, U> {
    /// Get a reference to the pubsub client for tests and dev tooling.
    pub fn pubsub_client(&self) -> &U {
        &self.pubsub_client
    }
}

impl<T: ChainRpcClient, U: ChainPubsubClient> RemoteAccountProvider<T, U> {
    /// Check if an account is currently pending (being fetched).
    pub(crate) fn is_pending(&self, pubkey: &Pubkey) -> bool {
        let fetching = self
            .fetching_accounts
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        fetching.contains_key(pubkey)
    }
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
