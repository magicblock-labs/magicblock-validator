use std::{
    collections::{hash_map::Entry, HashMap, HashSet},
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
pub use lru_cache::{AccountsLruCache, AddAccountOutcome};
use magicblock_config::config::GrpcConfig;
pub(crate) use remote_account::RemoteAccount;
pub use remote_account::RemoteAccountUpdateSource;
use solana_commitment_config::CommitmentConfig;
use solana_pubkey::Pubkey;
use tokio::{
    sync::{mpsc, oneshot, Mutex as AsyncMutex, Notify},
    task,
};
use tracing::*;

pub mod chain_slot;
mod provider_fetch;
mod provider_setup;
mod provider_subscriptions;
mod provider_updates;
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
use magicblock_metrics::metrics::{
    dec_chainlink_pending_fetch_waiters_gauge,
    inc_chainlink_subscription_cleanup_accounts,
    inc_chainlink_subscription_registration_accounts,
    observe_chainlink_companion_fetch_attempts,
    observe_chainlink_companion_fetch_duration_seconds,
    observe_chainlink_pending_fetch_owner_duration_seconds_with_context,
    AccountFetchContext, ChainlinkCompanionFetchKind,
    ChainlinkCompanionFetchOutcome, ChainlinkPendingFetchLayer,
    ChainlinkPendingFetchOutcome, SubscriptionCleanupOutcome,
    SubscriptionCleanupSource, SubscriptionReasonLabel,
    SubscriptionRegistrationOrigin, SubscriptionRegistrationOutcome,
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
    /// cleanup is deferred to a detached task.
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

        let (_confirmed_missing, evicted) = {
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
