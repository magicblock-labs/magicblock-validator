use std::{
    collections::HashSet,
    sync::{atomic::AtomicU16, Arc},
};

use magicblock_core::logger::log_trace_warn;
use magicblock_metrics::metrics::{
    inc_chainlink_subscription_cleanup_accounts, SubscriptionCleanupOutcome,
    SubscriptionCleanupSource,
};
use solana_pubkey::Pubkey;
use tokio::sync::{mpsc, Mutex as AsyncMutex};
use tracing::*;

#[cfg(test)]
use super::PendingFetchCoverageRequirement;
use super::{
    subscription_key_owned_guard_from_map,
    AccountSubscriptionPublicationPolicy, AccountsLruCache, ChainPubsubClient,
    FetchingAccounts, PendingAccountSubscriptionPublication, PubsubTransport,
    SharedCapacityEvictionProtectionPredicate, SubscriptionKeyGuard,
    SubscriptionKeyLocks, SubscriptionOwnership, SubscriptionOwnershipMap,
    SubscriptionReason,
};
use crate::remote_account_provider::RemoteAccountProviderError;

// NOTE: Pubkey stringification overhead is acceptable here since this is a cold path
// (network I/O dwarfs the stringification cost)
#[instrument(skip(pubsub_client), fields(pubkey = %pubkey))]
async fn unsubscribe_for_removal<T: ChainPubsubClient>(
    pubkey: Pubkey,
    pubsub_client: &T,
    cleanup_source: SubscriptionCleanupSource,
) -> Option<SubscriptionCleanupOutcome> {
    match pubsub_client.unsubscribe(pubkey).await {
        Ok(()) => Some(SubscriptionCleanupOutcome::Unsubscribed),
        Err(err) => {
            if matches!(
                err,
                RemoteAccountProviderError::AccountSubscriptionDoesNotExist(_)
            ) {
                debug!(error = ?err, "Subscription is already absent");
                Some(SubscriptionCleanupOutcome::AlreadyAbsent)
            } else {
                warn!(error = ?err, "Failed to unsubscribe");
                inc_chainlink_subscription_cleanup_accounts(
                    cleanup_source,
                    SubscriptionCleanupOutcome::UnsubscribeFailed,
                );
                None
            }
        }
    }
}

struct PendingRemovalNotification {
    pubkey: Pubkey,
    removed_account_tx: mpsc::Sender<Pubkey>,
    cleanup_source: SubscriptionCleanupSource,
    cleanup_outcome: SubscriptionCleanupOutcome,
    pending: bool,
}

impl PendingRemovalNotification {
    async fn send(mut self) {
        let result = self.removed_account_tx.send(self.pubkey).await;
        self.pending = false;
        match result {
            Ok(()) => inc_chainlink_subscription_cleanup_accounts(
                self.cleanup_source,
                self.cleanup_outcome,
            ),
            Err(err) => {
                warn!(pubkey = %self.pubkey, error = ?err, "Failed to send removal update");
                inc_chainlink_subscription_cleanup_accounts(
                    self.cleanup_source,
                    SubscriptionCleanupOutcome::RemovalUpdateFailed,
                );
            }
        }
    }
}

impl Drop for PendingRemovalNotification {
    fn drop(&mut self) {
        if !self.pending {
            return;
        }
        let pubkey = self.pubkey;
        let removed_account_tx = self.removed_account_tx.clone();
        let cleanup_source = self.cleanup_source;
        let cleanup_outcome = self.cleanup_outcome;
        tokio::spawn(async move {
            match removed_account_tx.send(pubkey).await {
                Ok(()) => inc_chainlink_subscription_cleanup_accounts(
                    cleanup_source,
                    cleanup_outcome,
                ),
                Err(err) => {
                    warn!(pubkey = %pubkey, error = ?err, "Failed to send removal update");
                    inc_chainlink_subscription_cleanup_accounts(
                        cleanup_source,
                        SubscriptionCleanupOutcome::RemovalUpdateFailed,
                    );
                }
            }
        });
    }
}

async fn notify_removal(
    pubkey: Pubkey,
    removed_account_tx: &mpsc::Sender<Pubkey>,
    cleanup_source: SubscriptionCleanupSource,
    cleanup_outcome: SubscriptionCleanupOutcome,
) {
    PendingRemovalNotification {
        pubkey,
        removed_account_tx: removed_account_tx.clone(),
        cleanup_source,
        cleanup_outcome,
        pending: true,
    }
    .send()
    .await;
}

struct PendingStaleSubscriptionCleanup<T: ChainPubsubClient> {
    pubkey: Pubkey,
    pubsub_client: Option<T>,
    removed_account_tx: Option<mpsc::Sender<Pubkey>>,
    subscription_guard: Option<SubscriptionKeyGuard>,
    pending: bool,
}

impl<T: ChainPubsubClient> PendingStaleSubscriptionCleanup<T> {
    fn new(
        pubkey: Pubkey,
        pubsub_client: &T,
        removed_account_tx: &mpsc::Sender<Pubkey>,
        subscription_guard: Option<SubscriptionKeyGuard>,
    ) -> Self {
        pubsub_client.revoke_grpc_subscription_preference(&pubkey);
        Self {
            pubkey,
            pubsub_client: Some(pubsub_client.clone()),
            removed_account_tx: Some(removed_account_tx.clone()),
            subscription_guard,
            pending: true,
        }
    }

    async fn finish(mut self) {
        let cleanup_outcome = unsubscribe_for_removal(
            self.pubkey,
            self.pubsub_client
                .as_ref()
                .expect("pending cleanup client must exist"),
            SubscriptionCleanupSource::Reconciler,
        )
        .await;
        if let Some(cleanup_outcome) = cleanup_outcome {
            self.pubsub_client
                .as_ref()
                .expect("pending cleanup client must exist")
                .finalize_subscription_removal(&self.pubkey);
            drop(self.subscription_guard.take());
            self.pending = false;
            let removed_account_tx = self
                .removed_account_tx
                .take()
                .expect("pending cleanup sender must exist");
            notify_removal(
                self.pubkey,
                &removed_account_tx,
                SubscriptionCleanupSource::Reconciler,
                cleanup_outcome,
            )
            .await;
        } else {
            drop(self.subscription_guard.take());
            self.pending = false;
        }
    }
}

impl<T: ChainPubsubClient> Drop for PendingStaleSubscriptionCleanup<T> {
    fn drop(&mut self) {
        if !self.pending {
            return;
        }
        let pubkey = self.pubkey;
        let Some(pubsub_client) = self.pubsub_client.take() else {
            return;
        };
        let Some(removed_account_tx) = self.removed_account_tx.take() else {
            return;
        };
        let subscription_guard = self.subscription_guard.take();
        tokio::spawn(async move {
            pubsub_client.revoke_grpc_subscription_preference(&pubkey);
            let cleanup_outcome = unsubscribe_for_removal(
                pubkey,
                &pubsub_client,
                SubscriptionCleanupSource::Reconciler,
            )
            .await;
            if let Some(cleanup_outcome) = cleanup_outcome {
                pubsub_client.finalize_subscription_removal(&pubkey);
                drop(subscription_guard);
                notify_removal(
                    pubkey,
                    &removed_account_tx,
                    SubscriptionCleanupSource::Reconciler,
                    cleanup_outcome,
                )
                .await;
            }
        });
    }
}

fn is_eviction_protected(
    pubkey: &Pubkey,
    ownership: Option<&SubscriptionOwnership>,
    fetching_accounts: Option<&FetchingAccounts>,
    capacity_eviction_protection: Option<
        &SharedCapacityEvictionProtectionPredicate,
    >,
) -> bool {
    fetching_accounts.is_some_and(|fetching_accounts| {
        fetching_accounts
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .contains_key(pubkey)
    }) || capacity_eviction_protection.is_some_and(|protection| {
        protection
            .read()
            .unwrap_or_else(|poison| poison.into_inner())
            .as_ref()
            .is_some_and(|predicate| predicate(pubkey).is_protected())
    }) || ownership.is_some_and(|ownership| {
        ownership.contains(SubscriptionReason::UndelegationTracking)
    })
}

/// Reconciles subscription state between the LRU cache and the pubsub client.
///
/// This function is called when a mismatch is detected between the accounts
/// tracked in the LRU cache and the actual subscriptions held by the pubsub
/// client. It repairs drift by:
///
/// - **Resubscribing**: Accounts present in the LRU cache but missing from the
///   pubsub client are resubscribed. This can happen if subscriptions were
///   dropped due to network issues or reconnects.
///
/// - **Unsubscribing**: Accounts present in the pubsub client but missing from
///   the LRU cache are unsubscribed and reported through `removed_account_tx`.
///   This can happen if the LRU evicted an account but the unsubscribe request
///   failed or was lost.
///
/// `never_evicted` contains system accounts, such as sysvars, that are expected
/// to be present in pubsub without being tracked in the LRU. These are excluded
/// so reconciliation does not incorrectly unsubscribe them.
///
/// When `subscription_key_locks` is provided, reconciliation serializes each
/// per-pubkey repair with normal subscription transitions and rechecks the live
/// LRU/pubsub state after acquiring the lock. This prevents a stale snapshot
/// from unsubscribing an account that is in the middle of being registered, while
/// still allowing tests to exercise the unlocked reconciliation path by passing
/// `None`.
/// When `subscription_transition_lock` is provided, composite LRU/ownership
/// removals use the same short critical section as normal tier transitions.
///
/// Returns the expected total number of monitored accounts: LRU-tracked accounts
/// plus never-evicted accounts.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn reconcile_subscriptions<PubsubClient: ChainPubsubClient>(
    subscribed_accounts: &AccountsLruCache,
    secondary_subscriptions: &AccountsLruCache,
    pubsub_client: &PubsubClient,
    never_evicted: &[Pubkey],
    removed_account_tx: &mpsc::Sender<Pubkey>,
    subscription_key_locks: Option<&SubscriptionKeyLocks>,
    subscription_transition_lock: Option<&Arc<AsyncMutex<()>>>,
    subscription_ownership: Option<&SubscriptionOwnershipMap>,
    fetching_accounts: Option<&FetchingAccounts>,
    capacity_eviction_protection: Option<
        &SharedCapacityEvictionProtectionPredicate,
    >,
) -> usize {
    let lru_pubkeys = subscribed_accounts.pubkeys();
    let lru_count = lru_pubkeys.len();
    let secondary_pubkeys = secondary_subscriptions.pubkeys();
    let expected_total =
        lru_count + never_evicted.len() + secondary_pubkeys.len();

    let Some(pubsub_snapshot) =
        pubsub_client.subscription_reconciliation_snapshot()
    else {
        debug!(
            lru_count = lru_count,
            never_evicted_count = never_evicted.len(),
            "Skipping subscription reconciliation because no connected pubsub client is available"
        );
        return expected_total;
    };

    // Never-evicted keys (e.g. the clock sysvar) are not tracked in the LRU,
    // so the LRU diff below cannot repair them - collect the ones missing
    // from any client to resubscribe them directly.
    let missing_never_evicted: Vec<Pubkey> = never_evicted
        .iter()
        .filter(|pk| !pubsub_snapshot.intersection.contains(pk))
        .copied()
        .collect();

    let ensured_subs_without_never_evict: HashSet<_> = pubsub_snapshot
        .intersection
        .iter()
        .copied()
        .filter(|pk| !never_evicted.contains(pk))
        .collect();
    let partial_subs_without_never_evict: HashSet<_> = pubsub_snapshot
        .union
        .iter()
        .copied()
        .filter(|pk| !never_evicted.contains(pk))
        .collect();

    // A) LRU subs that are not ensured by all clients
    let extra_in_lru: HashSet<_> = lru_pubkeys
        .difference(&ensured_subs_without_never_evict)
        .collect();
    // B) Subs not in either tier that some clients hold
    let extra_in_pubsub: HashSet<_> = partial_subs_without_never_evict
        .difference(&lru_pubkeys)
        .filter(|pk| !secondary_pubkeys.contains(pk))
        .collect();
    // C) Every secondary account prefers gRPC-only coverage. When gRPC is
    // unavailable, retain full coverage.
    let grpc_snapshot = pubsub_client
        .subscription_reconciliation_snapshot_for_transport(
            PubsubTransport::Grpc,
        );
    let websocket_snapshot = pubsub_client
        .subscription_reconciliation_snapshot_for_transport(
            PubsubTransport::WebSocket,
        );
    let pending_full_coverage: HashSet<Pubkey> = fetching_accounts
        .map(|fetching_accounts| {
            fetching_accounts
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .iter()
                .filter_map(|(pubkey, state)| {
                    state
                        .requires_full_coverage
                        .requires_full()
                        .then_some(*pubkey)
                })
                .collect()
        })
        .unwrap_or_default();
    let secondary_to_repair: Vec<Pubkey> = secondary_pubkeys
        .iter()
        .filter(|pubkey| {
            if pending_full_coverage.contains(pubkey) {
                !pubsub_snapshot.intersection.contains(pubkey)
            } else {
                match &grpc_snapshot {
                    Some(grpc) => {
                        !grpc.intersection.contains(pubkey)
                            || websocket_snapshot
                                .as_ref()
                                .is_some_and(|ws| ws.union.contains(pubkey))
                    }
                    None => !pubsub_snapshot.intersection.contains(pubkey),
                }
            }
        })
        .copied()
        .collect();

    trace!(
        lru_count = lru_count,
        ensured_count = ensured_subs_without_never_evict.len(),
        partial_count = partial_subs_without_never_evict.len(),
        extra_in_lru_count = extra_in_lru.len(),
        extra_in_pubsub_count = extra_in_pubsub.len(),
        "Reconciling subscriptions between LRU and pubsub client"
    );

    // Resubscribe never-evicted keys missing from any client. Clients that
    // already hold the subscription dedup the call.
    if !missing_never_evicted.is_empty() {
        warn!(
            pubkeys = ?missing_never_evicted,
            "Resubscribing missing never-evicted accounts"
        );
        for pubkey in missing_never_evicted {
            let _subscription_guard =
                acquire_subscription_key_guard(subscription_key_locks, pubkey)
                    .await;
            if let Err(e) = pubsub_client.subscribe(pubkey, None).await {
                warn!(
                    pubkey = %pubkey,
                    error = ?e,
                    "Failed to resubscribe never-evicted account"
                );
            }
        }
    }

    // For any sub that is in the LRU but not ensured by all clients we resubscribe.
    // This may call subscribe on some clients that already have the subscription and
    // is ignored by that client.
    if !extra_in_lru.is_empty() {
        static LOG_TRACE_COUNT: AtomicU16 = AtomicU16::new(0);
        // If this happens a lot then this is serious since that means that some clients
        // were not subscribed to all accounts
        let len = extra_in_lru.len();
        let err = RemoteAccountProviderError::AccountSubscriptionsOutOfSync(
            format!("{len} accounts in LRU but not in pubsub"),
        );
        log_trace_warn(
            "Consolidating missing subscriptions",
            "Consolidated missing subscriptions repeatedly",
            &len.to_string(),
            &err,
            100,
            &LOG_TRACE_COUNT,
        );
        trace!(pubkeys = ?extra_in_lru, "Resubscribing missing accounts");
        for pubkey in extra_in_lru {
            let pubkey = *pubkey;
            let _subscription_guard =
                acquire_subscription_key_guard(subscription_key_locks, pubkey)
                    .await;

            if !subscribed_accounts.contains(&pubkey) {
                trace!(
                    pubkey = %pubkey,
                    "Skipping resubscribe because pubkey left LRU after reconciliation snapshot"
                );
                continue;
            }

            let Some(pubsub_snapshot) =
                pubsub_client.subscription_reconciliation_snapshot()
            else {
                trace!(
                    pubkey = %pubkey,
                    "Skipping resubscribe because no connected pubsub client is available after reconciliation snapshot"
                );
                continue;
            };

            if pubsub_snapshot.intersection.contains(&pubkey) {
                trace!(
                    pubkey = %pubkey,
                    "Skipping resubscribe because pubkey is already ensured after reconciliation snapshot"
                );
                continue;
            }

            let publication = PendingAccountSubscriptionPublication::new(
                pubsub_client,
                pubkey,
                AccountSubscriptionPublicationPolicy::Full,
            );
            let resub_err = pubsub_client.subscribe(pubkey, None).await.err();
            if let Some(err) = &resub_err {
                warn!(pubkey = %pubkey, error = ?err, "Failed to resubscribe account");
            }

            // A subscription on any client keeps the account fresh; later
            // cycles repair the remaining clients.
            let delivered_somewhere = pubsub_client
                .subscription_reconciliation_snapshot()
                .is_some_and(|s| s.union.contains(&pubkey));
            if delivered_somewhere {
                publication.commit();
                continue;
            }

            // No client holds the subscription: evict so the account is
            // re-cloned fresh on next use instead of going stale.
            let removed = {
                let _transition_guard = match subscription_transition_lock {
                    Some(lock) => Some(lock.lock().await),
                    None => None,
                };

                if !subscribed_accounts.contains(&pubkey) {
                    false
                } else {
                    let mut ownership = match subscription_ownership {
                        Some(ownership) => Some(ownership.lock().await),
                        None => None,
                    };
                    let is_protected = is_eviction_protected(
                        &pubkey,
                        ownership
                            .as_ref()
                            .and_then(|ownership| ownership.get(&pubkey)),
                        fetching_accounts,
                        capacity_eviction_protection,
                    );

                    if is_protected {
                        false
                    } else {
                        warn!(
                            pubkey = %pubkey,
                            error = ?resub_err,
                            "Resubscribe did not take effect on any client; evicting account to prevent stale state"
                        );
                        publication.update(
                            AccountSubscriptionPublicationPolicy::Absent,
                        );
                        pubsub_client
                            .revoke_grpc_subscription_preference(&pubkey);
                        subscribed_accounts.remove(&pubkey);
                        if let Some(ownership) = ownership.as_mut() {
                            ownership.remove(&pubkey);
                        }
                        pubsub_client.finalize_subscription_removal(&pubkey);
                        true
                    }
                }
            };
            if !removed {
                continue;
            }
            publication.commit();
            drop(_subscription_guard);
            notify_removal(
                pubkey,
                removed_account_tx,
                SubscriptionCleanupSource::Reconciler,
                SubscriptionCleanupOutcome::Unsubscribed,
            )
            .await;
        }
    }

    // For any sub that is in any client but not in the LRU we unsubscribe and trigger a removal
    // notification.
    // This may call unsubscribe on some clients that don't have the subscription and
    // is ignored by that client.
    if !extra_in_pubsub.is_empty() {
        debug!(
            count = extra_in_pubsub.len(),
            "Unsubscribing accounts in pubsub but not in LRU"
        );
        trace!(pubkeys = ?extra_in_pubsub, "Unsubscribing stale accounts");
        for pubkey in extra_in_pubsub {
            let pubkey = *pubkey;
            let _subscription_guard =
                acquire_subscription_key_guard(subscription_key_locks, pubkey)
                    .await;

            if subscribed_accounts.contains(&pubkey)
                || secondary_subscriptions.contains(&pubkey)
            {
                trace!(
                    pubkey = %pubkey,
                    "Skipping stale unsubscribe because pubkey entered LRU after reconciliation snapshot"
                );
                continue;
            }

            let Some(pubsub_snapshot) =
                pubsub_client.subscription_reconciliation_snapshot()
            else {
                trace!(
                    pubkey = %pubkey,
                    "Skipping stale unsubscribe because no connected pubsub client is available after reconciliation snapshot"
                );
                continue;
            };

            if !pubsub_snapshot.union.contains(&pubkey) {
                trace!(
                    pubkey = %pubkey,
                    "Skipping stale unsubscribe because pubkey left pubsub after reconciliation snapshot"
                );
                continue;
            }

            let claimed_for_cleanup = {
                let _transition_guard = match subscription_transition_lock {
                    Some(lock) => Some(lock.lock().await),
                    None => None,
                };
                if subscribed_accounts.contains(&pubkey)
                    || secondary_subscriptions.contains(&pubkey)
                {
                    false
                } else {
                    let mut ownership = match subscription_ownership {
                        Some(ownership) => Some(ownership.lock().await),
                        None => None,
                    };
                    let is_protected = is_eviction_protected(
                        &pubkey,
                        ownership
                            .as_ref()
                            .and_then(|ownership| ownership.get(&pubkey)),
                        fetching_accounts,
                        capacity_eviction_protection,
                    );
                    if is_protected {
                        false
                    } else {
                        if let Some(ownership) = ownership.as_mut() {
                            ownership.remove(&pubkey);
                        }
                        true
                    }
                }
            };
            if !claimed_for_cleanup {
                trace!(
                    pubkey = %pubkey,
                    "Skipping stale unsubscribe because account became authoritative or protected"
                );
                continue;
            }

            PendingStaleSubscriptionCleanup::new(
                pubkey,
                pubsub_client,
                removed_account_tx,
                _subscription_guard,
            )
            .finish()
            .await;
        }
    }
    // Restore the desired transport coverage for secondary accounts.
    for pubkey in secondary_to_repair {
        let _subscription_guard =
            acquire_subscription_key_guard(subscription_key_locks, pubkey)
                .await;

        if !secondary_subscriptions.contains(&pubkey) {
            continue;
        }

        let requires_full_coverage =
            fetching_accounts.is_some_and(|fetching_accounts| {
                fetching_accounts
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner())
                    .get(&pubkey)
                    .is_some_and(|state| {
                        state.requires_full_coverage.requires_full()
                    })
            });
        let publication = PendingAccountSubscriptionPublication::new(
            pubsub_client,
            pubkey,
            if requires_full_coverage {
                AccountSubscriptionPublicationPolicy::Full
            } else {
                AccountSubscriptionPublicationPolicy::GrpcPreferred
            },
        );
        let covered = if requires_full_coverage {
            if let Err(err) = pubsub_client.subscribe(pubkey, None).await {
                warn!(pubkey = %pubkey, error = ?err, "Failed to restore full coverage for pending secondary account");
            }
            pubsub_client
                .subscription_reconciliation_snapshot()
                .is_some_and(|snapshot| snapshot.intersection.contains(&pubkey))
        } else {
            let preferred_result =
                pubsub_client.prefer_grpc_subscription(pubkey).await;
            if let Err(err) = &preferred_result {
                debug!(pubkey = %pubkey, error = ?err, "Secondary repair failed; restoring full coverage");
            }
            let preferred_covered = pubsub_client.is_subscribed(&pubkey);
            if preferred_result.is_err() || !preferred_covered {
                publication.update(AccountSubscriptionPublicationPolicy::Full);
                if let Err(err) = pubsub_client.subscribe(pubkey, None).await {
                    warn!(pubkey = %pubkey, error = ?err, "Failed to repair secondary account subscription");
                }
            }
            pubsub_client.is_subscribed(&pubkey)
        };
        if covered {
            publication.commit();
            continue;
        }

        let removed = {
            let _transition_guard = match subscription_transition_lock {
                Some(lock) => Some(lock.lock().await),
                None => None,
            };

            if !secondary_subscriptions.contains(&pubkey) {
                false
            } else {
                let mut ownership = match subscription_ownership {
                    Some(ownership) => Some(ownership.lock().await),
                    None => None,
                };
                let is_protected = is_eviction_protected(
                    &pubkey,
                    ownership
                        .as_ref()
                        .and_then(|ownership| ownership.get(&pubkey)),
                    fetching_accounts,
                    capacity_eviction_protection,
                );

                if is_protected {
                    false
                } else {
                    warn!(pubkey = %pubkey, "Secondary repair did not take effect on any client; evicting account to prevent stale state");
                    pubsub_client.revoke_grpc_subscription_preference(&pubkey);
                    publication
                        .update(AccountSubscriptionPublicationPolicy::Absent);
                    secondary_subscriptions.remove(&pubkey);
                    if let Some(ownership) = ownership.as_mut() {
                        ownership.remove(&pubkey);
                    }
                    pubsub_client.finalize_subscription_removal(&pubkey);
                    true
                }
            }
        };
        if !removed {
            continue;
        }
        publication.commit();
        drop(_subscription_guard);
        notify_removal(
            pubkey,
            removed_account_tx,
            SubscriptionCleanupSource::Reconciler,
            SubscriptionCleanupOutcome::Unsubscribed,
        )
        .await;
    }

    // We assume that reconciling worked and now our subscribed accounts are up to date
    // Pubsubs should be subscribed to all accounts in LRU accounts and accounts that
    // are never evicted (not tracked in LRU)
    expected_total
}

async fn acquire_subscription_key_guard(
    subscription_key_locks: Option<&SubscriptionKeyLocks>,
    pubkey: Pubkey,
) -> Option<SubscriptionKeyGuard> {
    match subscription_key_locks {
        Some(subscription_key_locks) => Some(
            subscription_key_owned_guard_from_map(
                subscription_key_locks,
                pubkey,
            )
            .await,
        ),
        None => None,
    }
}

#[cfg(test)]
mod tests {
    use std::{num::NonZeroUsize, sync::Arc, time::Duration};

    use solana_pubkey::Pubkey;
    use tokio::sync::mpsc;

    use super::*;
    use crate::{
        remote_account_provider::{
            chain_pubsub_client::{
                mock::ChainPubsubClientMock, PubsubTransport,
            },
            lru_cache::AccountsLruCache,
            pubsub_common::SubscriptionUpdate,
            CapacityEvictionProtection, FetchingAccountState,
        },
        testing::init_logger,
        AccountFetchContext,
    };

    fn create_test_pubkey(seed: u8) -> Pubkey {
        let mut bytes = [0u8; 32];
        bytes[0] = seed;
        Pubkey::from(bytes)
    }

    #[tokio::test]
    async fn test_dead_primary_removal_is_atomic_and_releases_key_before_notify(
    ) {
        init_logger();

        let (tx, rx) = mpsc::channel::<SubscriptionUpdate>(10);
        let mock_client = ChainPubsubClientMock::new(tx, rx);
        let pubkey = create_test_pubkey(1);
        let lru =
            Arc::new(AccountsLruCache::new(NonZeroUsize::new(10).unwrap()));
        let secondary =
            Arc::new(AccountsLruCache::new(NonZeroUsize::new(10).unwrap()));
        lru.add(pubkey);
        let ownership: SubscriptionOwnershipMap = Default::default();
        ownership
            .lock()
            .await
            .entry(pubkey)
            .or_default()
            .acquire(SubscriptionReason::DirectAccount);
        let subscription_key_locks: SubscriptionKeyLocks = Default::default();
        let transition_lock = Arc::new(AsyncMutex::new(()));
        let transition_guard = transition_lock.lock().await;
        mock_client.silently_noop_next_subscriptions(1);

        // Keep the notification channel full. Normal reconciliation applies
        // bounded backpressure after releasing the key guard; cancellation
        // transfers only its one in-flight notification to recovery.
        let (removed_tx, mut removed_rx) = mpsc::channel::<Pubkey>(1);
        let sentinel = create_test_pubkey(2);
        removed_tx.send(sentinel).await.unwrap();

        let mut reconcile = tokio::spawn({
            let lru = lru.clone();
            let secondary = secondary.clone();
            let mock_client = mock_client.clone();
            let removed_tx = removed_tx.clone();
            let subscription_key_locks = subscription_key_locks.clone();
            let transition_lock = transition_lock.clone();
            let ownership = ownership.clone();
            async move {
                reconcile_subscriptions(
                    &lru,
                    &secondary,
                    &mock_client,
                    &[],
                    &removed_tx,
                    Some(&subscription_key_locks),
                    Some(&transition_lock),
                    Some(&ownership),
                    None,
                    None,
                )
                .await
            }
        });

        mock_client.wait_for_subscribe_attempts(1).await;
        // Paused time advances only after runnable work has quiesced, so the
        // reconciler has reached the held transition lock before these checks.
        tokio::time::pause();
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(lru.contains(&pubkey));
        assert!(ownership.lock().await.contains_key(&pubkey));
        tokio::time::resume();

        drop(transition_guard);
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if !lru.contains(&pubkey)
                    && !ownership.lock().await.contains_key(&pubkey)
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("reconciler should commit LRU and ownership removal");

        let key_guard = tokio::time::timeout(
            Duration::from_secs(1),
            subscription_key_owned_guard_from_map(
                &subscription_key_locks,
                pubkey,
            ),
        )
        .await
        .expect("notification backpressure must not retain the key guard");
        drop(key_guard);
        tokio::time::pause();
        assert!(
            tokio::time::timeout(Duration::from_millis(50), &mut reconcile,)
                .await
                .is_err(),
            "full notification channel should backpressure reconciliation"
        );
        tokio::time::resume();
        reconcile.abort();
        assert!(reconcile.await.unwrap_err().is_cancelled());

        assert_eq!(removed_rx.recv().await, Some(sentinel));
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), removed_rx.recv())
                .await
                .expect("cancelled notification recovery should complete"),
            Some(pubkey)
        );
        assert!(removed_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn test_already_absent_unsubscribe_still_notifies_removal() {
        init_logger();

        let (tx, rx) = mpsc::channel::<SubscriptionUpdate>(10);
        let mock_client = ChainPubsubClientMock::new(tx, rx);
        let pubkey = create_test_pubkey(1);
        let (removed_tx, mut removed_rx) = mpsc::channel::<Pubkey>(1);

        let cleanup_outcome = unsubscribe_for_removal(
            pubkey,
            &mock_client,
            SubscriptionCleanupSource::Reconciler,
        )
        .await
        .expect("already-absent is a terminal cleanup outcome");
        assert!(matches!(
            cleanup_outcome,
            SubscriptionCleanupOutcome::AlreadyAbsent
        ));
        notify_removal(
            pubkey,
            &removed_tx,
            SubscriptionCleanupSource::Reconciler,
            cleanup_outcome,
        )
        .await;

        assert_eq!(removed_rx.recv().await, Some(pubkey));
        assert!(removed_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn test_stale_pubsub_cleanup_claims_detached_ownership() {
        init_logger();

        let (tx, rx) = mpsc::channel::<SubscriptionUpdate>(10);
        let mock_client = ChainPubsubClientMock::new(tx, rx);
        let pubkey = create_test_pubkey(1);
        mock_client.insert_subscription(pubkey);
        let primary =
            Arc::new(AccountsLruCache::new(NonZeroUsize::new(10).unwrap()));
        let secondary =
            Arc::new(AccountsLruCache::new(NonZeroUsize::new(10).unwrap()));
        let ownership: SubscriptionOwnershipMap = Default::default();
        ownership
            .lock()
            .await
            .entry(pubkey)
            .or_default()
            .acquire(SubscriptionReason::DirectAccount);
        let key_locks: SubscriptionKeyLocks = Default::default();
        let transition_lock = Arc::new(AsyncMutex::new(()));
        let fetching_accounts = FetchingAccounts::default();
        let protection: SharedCapacityEvictionProtectionPredicate =
            Default::default();
        let (removed_tx, mut removed_rx) = mpsc::channel(1);

        reconcile_subscriptions(
            &primary,
            &secondary,
            &mock_client,
            &[],
            &removed_tx,
            Some(&key_locks),
            Some(&transition_lock),
            Some(&ownership),
            Some(&fetching_accounts),
            Some(&protection),
        )
        .await;

        assert!(!ownership.lock().await.contains_key(&pubkey));
        assert!(!mock_client.is_subscribed(&pubkey));
        assert_eq!(removed_rx.recv().await, Some(pubkey));
        assert!(removed_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn test_cancelled_stale_pubsub_cleanup_finishes_and_notifies() {
        init_logger();

        let (tx, rx) = mpsc::channel::<SubscriptionUpdate>(10);
        let mock_client = ChainPubsubClientMock::new(tx, rx);
        let pubkey = create_test_pubkey(1);
        mock_client.insert_subscription(pubkey);
        mock_client.block_unsubscribe();
        let primary =
            Arc::new(AccountsLruCache::new(NonZeroUsize::new(10).unwrap()));
        let secondary =
            Arc::new(AccountsLruCache::new(NonZeroUsize::new(10).unwrap()));
        let ownership: SubscriptionOwnershipMap = Default::default();
        ownership
            .lock()
            .await
            .entry(pubkey)
            .or_default()
            .acquire(SubscriptionReason::DirectAccount);
        let key_locks: SubscriptionKeyLocks = Default::default();
        let transition_lock = Arc::new(AsyncMutex::new(()));
        let fetching_accounts = FetchingAccounts::default();
        let protection: SharedCapacityEvictionProtectionPredicate =
            Default::default();
        let (removed_tx, mut removed_rx) = mpsc::channel(1);

        let reconcile = tokio::spawn({
            let primary = primary.clone();
            let secondary = secondary.clone();
            let mock_client = mock_client.clone();
            let ownership = ownership.clone();
            let key_locks = key_locks.clone();
            let transition_lock = transition_lock.clone();
            async move {
                reconcile_subscriptions(
                    &primary,
                    &secondary,
                    &mock_client,
                    &[],
                    &removed_tx,
                    Some(&key_locks),
                    Some(&transition_lock),
                    Some(&ownership),
                    Some(&fetching_accounts),
                    Some(&protection),
                )
                .await
            }
        });

        mock_client.wait_for_unsubscribe_attempts(1).await;
        assert!(!ownership.lock().await.contains_key(&pubkey));
        reconcile.abort();
        assert!(reconcile.await.unwrap_err().is_cancelled());

        mock_client.wait_for_unsubscribe_attempts(2).await;
        tokio::time::pause();
        assert!(
            tokio::time::timeout(
                Duration::from_millis(50),
                subscription_key_owned_guard_from_map(&key_locks, pubkey),
            )
            .await
            .is_err(),
            "cleanup recovery must retain the per-key guard"
        );
        tokio::time::resume();

        mock_client.release_unsubscribe();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), removed_rx.recv())
                .await
                .expect("cleanup recovery should notify removal"),
            Some(pubkey)
        );
        assert!(!mock_client.is_subscribed(&pubkey));
        let key_guard = tokio::time::timeout(
            Duration::from_secs(1),
            subscription_key_owned_guard_from_map(&key_locks, pubkey),
        )
        .await
        .expect("cleanup recovery should release the per-key guard");
        drop(key_guard);
    }

    #[tokio::test]
    async fn test_dead_secondary_rechecks_fetch_protection_under_transition_lock(
    ) {
        init_logger();

        let (tx, rx) = mpsc::channel::<SubscriptionUpdate>(10);
        let mock_client = ChainPubsubClientMock::new(tx, rx);
        mock_client.set_transport(PubsubTransport::Grpc);
        let pubkey = create_test_pubkey(1);
        let primary =
            Arc::new(AccountsLruCache::new(NonZeroUsize::new(10).unwrap()));
        let secondary =
            Arc::new(AccountsLruCache::new(NonZeroUsize::new(10).unwrap()));
        secondary.add(pubkey);
        let ownership: SubscriptionOwnershipMap = Default::default();
        ownership
            .lock()
            .await
            .entry(pubkey)
            .or_default()
            .acquire(SubscriptionReason::DirectAccount);
        let fetching_accounts = Arc::new(FetchingAccounts::default());
        let capacity_eviction_protection:
            SharedCapacityEvictionProtectionPredicate = Default::default();
        let subscription_key_locks: SubscriptionKeyLocks = Default::default();
        let transition_lock = Arc::new(AsyncMutex::new(()));

        // Hold both locks so an implementation that checks protections
        // before the transition lock deterministically blocks on ownership
        // only after observing the old, unprotected fetching state.
        let transition_guard = transition_lock.lock().await;
        let ownership_guard = ownership.lock().await;
        mock_client.silently_noop_next_subscriptions(2);
        let (removed_tx, mut removed_rx) = mpsc::channel::<Pubkey>(1);

        let reconcile = tokio::spawn({
            let primary = primary.clone();
            let secondary = secondary.clone();
            let mock_client = mock_client.clone();
            let removed_tx = removed_tx.clone();
            let subscription_key_locks = subscription_key_locks.clone();
            let transition_lock = transition_lock.clone();
            let ownership = ownership.clone();
            let fetching_accounts = fetching_accounts.clone();
            let capacity_eviction_protection =
                capacity_eviction_protection.clone();
            async move {
                reconcile_subscriptions(
                    &primary,
                    &secondary,
                    &mock_client,
                    &[],
                    &removed_tx,
                    Some(&subscription_key_locks),
                    Some(&transition_lock),
                    Some(&ownership),
                    Some(fetching_accounts.as_ref()),
                    Some(&capacity_eviction_protection),
                )
                .await
            }
        });

        tokio::time::timeout(
            Duration::from_secs(1),
            mock_client.wait_for_subscribe_attempts(2),
        )
        .await
        .expect("secondary repair should attempt both preferred and fallback coverage");
        tokio::task::yield_now().await;
        fetching_accounts.lock().unwrap().insert(
            pubkey,
            FetchingAccountState {
                generation: 1,
                fetch_start_slot: 0,
                fetch_context: AccountFetchContext::rpc_get_account(),
                requires_full_coverage: Arc::new(
                    PendingFetchCoverageRequirement::new(false),
                ),
                owner_started_at: std::time::Instant::now(),
                waiters: Vec::new(),
            },
        );
        drop(ownership_guard);
        drop(transition_guard);

        tokio::time::timeout(Duration::from_secs(1), reconcile)
            .await
            .expect("reconciliation should complete")
            .unwrap();
        assert!(secondary.contains(&pubkey));
        assert!(ownership.lock().await.contains_key(&pubkey));
        assert!(removed_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn test_dead_primary_respects_fetch_and_capacity_protections() {
        init_logger();

        let (tx, rx) = mpsc::channel::<SubscriptionUpdate>(10);
        let mock_client = ChainPubsubClientMock::new(tx, rx);
        let pubkey = create_test_pubkey(1);
        let primary = AccountsLruCache::new(NonZeroUsize::new(10).unwrap());
        let secondary = AccountsLruCache::new(NonZeroUsize::new(10).unwrap());
        primary.add(pubkey);
        let ownership: SubscriptionOwnershipMap = Default::default();
        ownership
            .lock()
            .await
            .entry(pubkey)
            .or_default()
            .acquire(SubscriptionReason::DirectAccount);
        let fetching_accounts = FetchingAccounts::default();
        let capacity_eviction_protection:
            SharedCapacityEvictionProtectionPredicate = Default::default();
        let (removed_tx, mut removed_rx) = mpsc::channel::<Pubkey>(10);

        fetching_accounts.lock().unwrap().insert(
            pubkey,
            FetchingAccountState {
                generation: 1,
                fetch_start_slot: 0,
                fetch_context: AccountFetchContext::rpc_get_account(),
                requires_full_coverage: Arc::new(
                    PendingFetchCoverageRequirement::new(false),
                ),
                owner_started_at: std::time::Instant::now(),
                waiters: Vec::new(),
            },
        );
        mock_client.silently_noop_next_subscriptions(1);
        reconcile_subscriptions(
            &primary,
            &secondary,
            &mock_client,
            &[],
            &removed_tx,
            None,
            None,
            Some(&ownership),
            Some(&fetching_accounts),
            Some(&capacity_eviction_protection),
        )
        .await;
        assert!(primary.contains(&pubkey));
        assert!(ownership.lock().await.contains_key(&pubkey));
        assert!(removed_rx.try_recv().is_err());

        fetching_accounts.lock().unwrap().remove(&pubkey);
        *capacity_eviction_protection.write().unwrap() =
            Some(Arc::new(|_| CapacityEvictionProtection {
                delegated: true,
                undelegating: false,
            }));
        mock_client.silently_noop_next_subscriptions(1);
        reconcile_subscriptions(
            &primary,
            &secondary,
            &mock_client,
            &[],
            &removed_tx,
            None,
            None,
            Some(&ownership),
            Some(&fetching_accounts),
            Some(&capacity_eviction_protection),
        )
        .await;
        assert!(primary.contains(&pubkey));
        assert!(ownership.lock().await.contains_key(&pubkey));
        assert!(removed_rx.try_recv().is_err());

        *capacity_eviction_protection.write().unwrap() = None;
        mock_client.silently_noop_next_subscriptions(1);
        reconcile_subscriptions(
            &primary,
            &secondary,
            &mock_client,
            &[],
            &removed_tx,
            None,
            None,
            Some(&ownership),
            Some(&fetching_accounts),
            Some(&capacity_eviction_protection),
        )
        .await;
        assert!(!primary.contains(&pubkey));
        assert!(!ownership.lock().await.contains_key(&pubkey));
        assert_eq!(removed_rx.try_recv(), Ok(pubkey));
    }

    /// Secondary-tier accounts in pubsub but not in the main LRU must not be
    /// unsubscribed as stale.
    #[tokio::test]
    async fn test_secondary_account_is_not_unsubscribed() {
        init_logger();

        let (tx, rx) = mpsc::channel::<SubscriptionUpdate>(10);
        let mock_client = ChainPubsubClientMock::new(tx, rx);
        mock_client.set_transport(PubsubTransport::Grpc);

        let pk = create_test_pubkey(1);
        mock_client.insert_subscription(pk);

        let lru = AccountsLruCache::new(NonZeroUsize::new(10).unwrap());
        let secondary = AccountsLruCache::new(NonZeroUsize::new(10).unwrap());
        secondary.add(pk);

        let (removed_tx, mut removed_rx) = mpsc::channel::<Pubkey>(10);
        reconcile_subscriptions(
            &lru,
            &secondary,
            &mock_client,
            &[],
            &removed_tx,
            None,
            None,
            None,
            None,
            None,
        )
        .await;

        assert!(mock_client.subscriptions_union().contains(&pk));
        assert!(removed_rx.try_recv().is_err());
    }

    /// Secondary-tier accounts no client holds get their gRPC coverage restored.
    #[tokio::test]
    async fn test_secondary_account_grpc_coverage_is_restored() {
        init_logger();

        let (tx, rx) = mpsc::channel::<SubscriptionUpdate>(10);
        let mock_client = ChainPubsubClientMock::new(tx, rx);
        mock_client.set_transport(PubsubTransport::Grpc);

        let pk = create_test_pubkey(1);
        let lru = AccountsLruCache::new(NonZeroUsize::new(10).unwrap());
        let secondary = AccountsLruCache::new(NonZeroUsize::new(10).unwrap());
        secondary.add(pk);

        let (removed_tx, mut removed_rx) = mpsc::channel::<Pubkey>(10);
        reconcile_subscriptions(
            &lru,
            &secondary,
            &mock_client,
            &[],
            &removed_tx,
            None,
            None,
            None,
            None,
            None,
        )
        .await;

        assert!(mock_client.subscriptions_union().contains(&pk));
        assert!(removed_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn test_secondary_noop_repair_respects_protections_then_evicts() {
        init_logger();

        let (tx, rx) = mpsc::channel::<SubscriptionUpdate>(10);
        let mock_client = ChainPubsubClientMock::new(tx, rx);
        mock_client.set_transport(PubsubTransport::Grpc);

        let pk = create_test_pubkey(1);
        let lru = AccountsLruCache::new(NonZeroUsize::new(10).unwrap());
        let secondary = AccountsLruCache::new(NonZeroUsize::new(10).unwrap());
        secondary.add(pk);
        let ownership: SubscriptionOwnershipMap = Default::default();
        ownership
            .lock()
            .await
            .entry(pk)
            .or_default()
            .acquire(SubscriptionReason::DirectAccount);
        let fetching_accounts = FetchingAccounts::default();
        let capacity_eviction_protection:
            SharedCapacityEvictionProtectionPredicate = Default::default();
        fetching_accounts.lock().unwrap().insert(
            pk,
            FetchingAccountState {
                generation: 1,
                fetch_start_slot: 0,
                fetch_context: AccountFetchContext::rpc_get_account(),
                requires_full_coverage: Arc::new(
                    PendingFetchCoverageRequirement::new(false),
                ),
                owner_started_at: std::time::Instant::now(),
                waiters: Vec::new(),
            },
        );
        mock_client.silently_noop_next_subscriptions(2);

        let (removed_tx, mut removed_rx) = mpsc::channel::<Pubkey>(10);
        reconcile_subscriptions(
            &lru,
            &secondary,
            &mock_client,
            &[],
            &removed_tx,
            None,
            None,
            Some(&ownership),
            Some(&fetching_accounts),
            Some(&capacity_eviction_protection),
        )
        .await;

        assert!(secondary.contains(&pk));
        assert!(ownership.lock().await.contains_key(&pk));
        assert!(removed_rx.try_recv().is_err());
        assert_eq!(mock_client.subscribe_attempts(), 2);

        fetching_accounts.lock().unwrap().remove(&pk);
        *capacity_eviction_protection.write().unwrap() =
            Some(Arc::new(|_| CapacityEvictionProtection {
                delegated: true,
                undelegating: false,
            }));
        mock_client.silently_noop_next_subscriptions(2);
        reconcile_subscriptions(
            &lru,
            &secondary,
            &mock_client,
            &[],
            &removed_tx,
            None,
            None,
            Some(&ownership),
            Some(&fetching_accounts),
            Some(&capacity_eviction_protection),
        )
        .await;

        assert!(secondary.contains(&pk));
        assert!(ownership.lock().await.contains_key(&pk));
        assert!(removed_rx.try_recv().is_err());
        assert_eq!(mock_client.subscribe_attempts(), 4);

        *capacity_eviction_protection.write().unwrap() = None;
        mock_client.silently_noop_next_subscriptions(2);
        reconcile_subscriptions(
            &lru,
            &secondary,
            &mock_client,
            &[],
            &removed_tx,
            None,
            None,
            Some(&ownership),
            Some(&fetching_accounts),
            Some(&capacity_eviction_protection),
        )
        .await;

        assert!(!secondary.contains(&pk));
        assert!(!ownership.lock().await.contains_key(&pk));
        assert_eq!(removed_rx.try_recv(), Ok(pk));
        assert_eq!(mock_client.subscribe_attempts(), 6);
    }

    #[tokio::test]
    async fn test_subs_in_lru_and_clients_same_noop() {
        init_logger();

        let (tx, rx) = mpsc::channel::<SubscriptionUpdate>(10);
        let mock_client = ChainPubsubClientMock::new(tx, rx);

        // Set up LRU with 2 accounts
        let lru = AccountsLruCache::new(NonZeroUsize::new(10).unwrap());
        let pk1 = create_test_pubkey(1);
        let pk2 = create_test_pubkey(2);

        lru.add(pk1);
        lru.add(pk2);

        // Set up client with same subscriptions
        mock_client.insert_subscription(pk1);
        mock_client.insert_subscription(pk2);

        // Create removal channel
        let (removed_tx, _removed_rx) = mpsc::channel::<Pubkey>(10);

        // Reconcile
        reconcile_subscriptions_local(&lru, &mock_client, &[], &removed_tx)
            .await;

        // Verify subscriptions are unchanged
        let subs = mock_client.subscriptions_union();
        assert_eq!(subs.len(), 2);
        assert!(subs.contains(&pk1));
        assert!(subs.contains(&pk2));
    }

    #[tokio::test]
    async fn test_not_all_lru_subs_ensured_resubscribes() {
        init_logger();

        let (tx, rx) = mpsc::channel::<SubscriptionUpdate>(10);
        let mock_client = ChainPubsubClientMock::new(tx, rx);

        // Set up LRU with 3 accounts
        let lru = AccountsLruCache::new(NonZeroUsize::new(10).unwrap());
        let pk1 = create_test_pubkey(1);
        let pk2 = create_test_pubkey(2);
        let pk3 = create_test_pubkey(3);

        lru.add(pk1);
        lru.add(pk2);
        lru.add(pk3);

        // Client only has pk1 and pk2
        mock_client.insert_subscription(pk1);
        mock_client.insert_subscription(pk2);

        // Create removal channel
        let (removed_tx, _removed_rx) = mpsc::channel::<Pubkey>(10);

        // Reconcile
        reconcile_subscriptions_local(&lru, &mock_client, &[], &removed_tx)
            .await;

        // Verify pk3 was resubscribed
        let subs = mock_client.subscriptions_union();
        assert_eq!(subs.len(), 3);
        assert!(subs.contains(&pk1));
        assert!(subs.contains(&pk2));
        assert!(subs.contains(&pk3));
    }

    #[tokio::test]
    async fn test_never_evicted_accounts_excluded() {
        init_logger();

        let (tx, rx) = mpsc::channel::<SubscriptionUpdate>(10);
        let mock_client = ChainPubsubClientMock::new(tx, rx);

        // Set up LRU with 2 accounts
        let lru = AccountsLruCache::new(NonZeroUsize::new(10).unwrap());
        let pk1 = create_test_pubkey(1);
        let pk2 = create_test_pubkey(2);
        let never_evict_pk = create_test_pubkey(99);

        lru.add(pk1);
        lru.add(pk2);

        // Client has all 3 subscriptions
        mock_client.insert_subscription(pk1);
        mock_client.insert_subscription(pk2);
        mock_client.insert_subscription(never_evict_pk);

        // Create removal channel
        let (removed_tx, mut removed_rx) = mpsc::channel::<Pubkey>(10);
        let never_evicted = vec![never_evict_pk];

        // Reconcile
        reconcile_subscriptions_local(
            &lru,
            &mock_client,
            &never_evicted,
            &removed_tx,
        )
        .await;

        // Verify never_evict_pk is still subscribed
        let subs = mock_client.subscriptions_union();
        assert_eq!(subs.len(), 3);
        assert!(subs.contains(&pk1));
        assert!(subs.contains(&pk2));
        assert!(subs.contains(&never_evict_pk));

        // Verify no removal notification for never_evict_pk
        assert!(removed_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn test_resubscribe_missing_account() {
        init_logger();

        let (tx, rx) = mpsc::channel::<SubscriptionUpdate>(10);
        let mock_client = ChainPubsubClientMock::new(tx, rx);

        // Set up LRU with pk1, pk2, pk3
        let lru = AccountsLruCache::new(NonZeroUsize::new(10).unwrap());
        let pk1 = create_test_pubkey(1);
        let pk2 = create_test_pubkey(2);
        let pk3 = create_test_pubkey(3);

        lru.add(pk1);
        lru.add(pk2);
        lru.add(pk3);

        // Client only has pk1 and pk2
        mock_client.insert_subscription(pk1);
        mock_client.insert_subscription(pk2);

        // Create removal channel
        let (removed_tx, _removed_rx) = mpsc::channel::<Pubkey>(10);

        // Reconcile
        reconcile_subscriptions_local(&lru, &mock_client, &[], &removed_tx)
            .await;

        // Verify pk3 was resubscribed
        let subs = mock_client.subscriptions_union();
        assert_eq!(subs.len(), 3);
        assert!(subs.contains(&pk1));
        assert!(subs.contains(&pk2));
        assert!(subs.contains(&pk3));
    }

    /// A successfully re-established subscription must not trigger eviction.
    #[tokio::test]
    async fn test_repaired_subscription_is_not_evicted() {
        init_logger();

        let (tx, rx) = mpsc::channel::<SubscriptionUpdate>(10);
        let mock_client = ChainPubsubClientMock::new(tx, rx);

        let lru = AccountsLruCache::new(NonZeroUsize::new(10).unwrap());
        let pk = create_test_pubkey(1);
        lru.add(pk);

        let (removed_tx, mut removed_rx) = mpsc::channel::<Pubkey>(10);
        reconcile_subscriptions_local(&lru, &mock_client, &[], &removed_tx)
            .await;

        assert!(mock_client.subscriptions_union().contains(&pk));
        assert!(lru.contains(&pk));
        assert!(removed_rx.try_recv().is_err());
    }

    /// When no client holds the subscription after the resubscribe, the
    /// account must be evicted instead of being left stale in the bank.
    #[tokio::test]
    async fn test_silently_dead_subscription_evicted_when_repair_fails() {
        init_logger();

        let (tx, rx) = mpsc::channel::<SubscriptionUpdate>(10);
        let mock_client = ChainPubsubClientMock::new(tx, rx);

        let lru = AccountsLruCache::new(NonZeroUsize::new(10).unwrap());
        let pk = create_test_pubkey(1);
        lru.add(pk);

        mock_client.silently_noop_next_subscriptions(1);

        let (removed_tx, mut removed_rx) = mpsc::channel::<Pubkey>(10);
        reconcile_subscriptions_local(&lru, &mock_client, &[], &removed_tx)
            .await;

        assert!(!mock_client.subscriptions_union().contains(&pk));
        assert!(!lru.contains(&pk));
        assert_eq!(removed_rx.try_recv(), Ok(pk));
    }

    /// Accounts owned for undelegation tracking must never be evicted; they
    /// are kept and retried on the next cycle.
    #[tokio::test]
    async fn test_undelegation_tracked_subscription_is_not_evicted() {
        init_logger();

        let (tx, rx) = mpsc::channel::<SubscriptionUpdate>(10);
        let mock_client = ChainPubsubClientMock::new(tx, rx);

        let lru = AccountsLruCache::new(NonZeroUsize::new(10).unwrap());
        let pk = create_test_pubkey(1);
        lru.add(pk);

        let ownership: SubscriptionOwnershipMap = Default::default();
        ownership
            .lock()
            .await
            .entry(pk)
            .or_default()
            .acquire(SubscriptionReason::UndelegationTracking);

        mock_client.silently_noop_next_subscriptions(1);

        let (removed_tx, mut removed_rx) = mpsc::channel::<Pubkey>(10);
        reconcile_subscriptions(
            &lru,
            &AccountsLruCache::new(NonZeroUsize::new(10).unwrap()),
            &mock_client,
            &[],
            &removed_tx,
            None,
            None,
            Some(&ownership),
            None,
            None,
        )
        .await;

        assert!(lru.contains(&pk));
        assert!(removed_rx.try_recv().is_err());
    }

    /// Test case: Empty LRU should cause resubscribe of all LRU accounts if missing
    /// Expected: No-op if pubsub is also empty (single client case)
    #[tokio::test]
    async fn test_empty_lru_empty_pubsub_noop() {
        init_logger();

        let (tx, rx) = mpsc::channel::<SubscriptionUpdate>(10);
        let mock_client = ChainPubsubClientMock::new(tx, rx);

        // Set up empty LRU
        let lru = AccountsLruCache::new(NonZeroUsize::new(10).unwrap());

        // Empty pubsub (single client case)
        // Create removal channel
        let (removed_tx, _removed_rx) = mpsc::channel::<Pubkey>(10);

        // Reconcile
        reconcile_subscriptions_local(&lru, &mock_client, &[], &removed_tx)
            .await;

        // Verify state unchanged (both empty)
        let subs = mock_client.subscriptions_union();
        assert_eq!(subs.len(), 0);
    }

    /// Test case: Empty pubsub with subscriptions in LRU
    /// Expected: Resubscribe to all accounts
    #[tokio::test]
    async fn test_empty_pubsub_resubscribes_all() {
        init_logger();

        let (tx, rx) = mpsc::channel::<SubscriptionUpdate>(10);
        let mock_client = ChainPubsubClientMock::new(tx, rx);

        // Set up LRU with 2 accounts
        let lru = AccountsLruCache::new(NonZeroUsize::new(10).unwrap());
        let pk1 = create_test_pubkey(1);
        let pk2 = create_test_pubkey(2);
        lru.add(pk1);
        lru.add(pk2);

        // Empty pubsub

        // Create removal channel
        let (removed_tx, _removed_rx) = mpsc::channel::<Pubkey>(10);

        // Reconcile
        reconcile_subscriptions_local(&lru, &mock_client, &[], &removed_tx)
            .await;

        // Verify all subscriptions added
        let subs = mock_client.subscriptions_union();
        assert_eq!(subs.len(), 2);
        assert!(subs.contains(&pk1));
        assert!(subs.contains(&pk2));
    }

    /// Test case: Multiple accounts missing from pubsub need resubscription
    /// Expected: All missing accounts get resubscribed
    #[tokio::test]
    async fn test_multiple_missing_accounts_resubscribed() {
        init_logger();

        let (tx, rx) = mpsc::channel::<SubscriptionUpdate>(10);
        let mock_client = ChainPubsubClientMock::new(tx, rx);

        // Set up LRU with pk1, pk2, pk3, pk4
        let lru = AccountsLruCache::new(NonZeroUsize::new(10).unwrap());
        let pk1 = create_test_pubkey(1);
        let pk2 = create_test_pubkey(2);
        let pk3 = create_test_pubkey(3);
        let pk4 = create_test_pubkey(4);

        lru.add(pk1);
        lru.add(pk2);
        lru.add(pk3);
        lru.add(pk4);

        // Client only has pk1 and pk2
        mock_client.insert_subscription(pk1);
        mock_client.insert_subscription(pk2);

        // Create removal channel
        let (removed_tx, _removed_rx) = mpsc::channel::<Pubkey>(10);

        // Reconcile
        reconcile_subscriptions_local(&lru, &mock_client, &[], &removed_tx)
            .await;

        // Verify all accounts are now subscribed
        let subs = mock_client.subscriptions_union();
        assert_eq!(subs.len(), 4);
        assert!(subs.contains(&pk1));
        assert!(subs.contains(&pk2));
        assert!(subs.contains(&pk3));
        assert!(subs.contains(&pk4));
    }

    #[tokio::test]
    async fn test_reconcile_resubscribes_accounts_missing_from_pubsub() {
        init_logger();

        let (tx, rx) = mpsc::channel(1);
        let pubsub_client = ChainPubsubClientMock::new(tx, rx);
        let (removed_tx, _removed_rx) = mpsc::channel(10);

        let capacity = NonZeroUsize::new(10).unwrap();
        let lru_cache = Arc::new(AccountsLruCache::new(capacity));

        let pubkey1 = create_test_pubkey(1);
        let pubkey2 = create_test_pubkey(2);
        let pubkey3 = create_test_pubkey(3);

        // Add accounts to LRU cache
        lru_cache.add(pubkey1);
        lru_cache.add(pubkey2);
        lru_cache.add(pubkey3);

        // Only pubkey1 is in pubsub (simulating missing subscriptions)
        pubsub_client.insert_subscription(pubkey1);

        let never_evicted: Vec<Pubkey> = vec![];

        // Reconcile should resubscribe pubkey2 and pubkey3
        reconcile_subscriptions_local(
            &lru_cache,
            &pubsub_client,
            &never_evicted,
            &removed_tx,
        )
        .await;

        // Verify all accounts are now subscribed
        let subs = pubsub_client.subscriptions_union();
        assert!(subs.contains(&pubkey1));
        assert!(subs.contains(&pubkey2));
        assert!(subs.contains(&pubkey3));
        assert_eq!(subs.len(), 3);
    }

    fn drain_removed_account_rx(
        rx: &mut mpsc::Receiver<Pubkey>,
    ) -> Vec<Pubkey> {
        let mut removed_accounts = Vec::new();
        while let Ok(pubkey) = rx.try_recv() {
            removed_accounts.push(pubkey);
        }
        removed_accounts
    }

    #[tokio::test]
    async fn test_reconcile_unsubscribes_accounts_not_in_lru() {
        init_logger();

        let (tx, rx) = mpsc::channel(1);
        let pubsub_client = ChainPubsubClientMock::new(tx, rx);
        let (removed_tx, mut removed_rx) = mpsc::channel(10);

        let capacity = NonZeroUsize::new(10).unwrap();
        let lru_cache = Arc::new(AccountsLruCache::new(capacity));

        let pubkey1 = create_test_pubkey(1);
        let pubkey2 = create_test_pubkey(2);
        let pubkey3 = create_test_pubkey(3);

        // Only pubkey1 is in LRU cache
        lru_cache.add(pubkey1);

        // All three are in pubsub (simulating stale subscriptions)
        pubsub_client.insert_subscription(pubkey1);
        pubsub_client.insert_subscription(pubkey2);
        pubsub_client.insert_subscription(pubkey3);

        let never_evicted: Vec<Pubkey> = vec![];

        // Reconcile should unsubscribe pubkey2 and pubkey3
        reconcile_subscriptions_local(
            &lru_cache,
            &pubsub_client,
            &never_evicted,
            &removed_tx,
        )
        .await;

        // Verify only pubkey1 remains subscribed
        let subs = pubsub_client.subscriptions_union();
        assert!(subs.contains(&pubkey1));
        assert!(!subs.contains(&pubkey2));
        assert!(!subs.contains(&pubkey3));
        assert_eq!(subs.len(), 1);

        // Verify removal notifications were sent for unsubscribed accounts
        let removed = drain_removed_account_rx(&mut removed_rx);
        assert_eq!(removed.len(), 2);
        assert!(removed.contains(&pubkey2));
        assert!(removed.contains(&pubkey3));
    }

    #[tokio::test]
    async fn test_reconcile_preserves_never_evicted_accounts_not_in_lru() {
        init_logger();

        let (tx, rx) = mpsc::channel(1);
        let pubsub_client = ChainPubsubClientMock::new(tx, rx);
        let (removed_tx, mut removed_rx) = mpsc::channel(10);

        let capacity = NonZeroUsize::new(10).unwrap();
        let lru_cache = Arc::new(AccountsLruCache::new(capacity));

        let pubkey_in_lru = create_test_pubkey(1);
        let never_evicted_pubkey = create_test_pubkey(2);
        let stale_pubkey = create_test_pubkey(3);

        // Only pubkey_in_lru is in LRU cache (never_evicted_pubkey is NOT in LRU)
        lru_cache.add(pubkey_in_lru);

        // All three are subscribed in pubsub
        pubsub_client.insert_subscription(pubkey_in_lru);
        pubsub_client.insert_subscription(never_evicted_pubkey);
        pubsub_client.insert_subscription(stale_pubkey);

        // never_evicted_pubkey is marked as never_evicted, so it should be
        // preserved even though it's not in the LRU cache
        let never_evicted = vec![never_evicted_pubkey];

        reconcile_subscriptions_local(
            &lru_cache,
            &pubsub_client,
            &never_evicted,
            &removed_tx,
        )
        .await;

        // Verify: pubkey_in_lru and never_evicted_pubkey remain, stale_pubkey
        // is unsubscribed
        let subs = pubsub_client.subscriptions_union();
        assert!(
            subs.contains(&pubkey_in_lru),
            "Account in LRU should remain subscribed"
        );
        assert!(
            subs.contains(&never_evicted_pubkey),
            "Never-evicted account should remain subscribed even if not in LRU"
        );
        assert!(
            !subs.contains(&stale_pubkey),
            "Stale account not in LRU and not never-evicted should be \
             unsubscribed"
        );
        assert_eq!(subs.len(), 2);

        // Verify removal notification was sent only for stale_pubkey
        let removed = drain_removed_account_rx(&mut removed_rx);
        assert_eq!(removed.len(), 1);
        assert!(removed.contains(&stale_pubkey));
    }

    async fn reconcile_subscriptions_local<PubsubClient: ChainPubsubClient>(
        subscribed_accounts: &AccountsLruCache,
        pubsub_client: &PubsubClient,
        never_evicted: &[Pubkey],
        removed_account_tx: &mpsc::Sender<Pubkey>,
    ) -> usize {
        let secondary = AccountsLruCache::new(NonZeroUsize::new(10).unwrap());
        reconcile_subscriptions(
            subscribed_accounts,
            &secondary,
            pubsub_client,
            never_evicted,
            removed_account_tx,
            None,
            None,
            None,
            None,
            None,
        )
        .await
    }
}
