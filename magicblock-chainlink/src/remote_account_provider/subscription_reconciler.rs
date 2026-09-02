use std::{collections::HashSet, sync::atomic::AtomicU16};

use magicblock_core::logger::log_trace_warn;
use magicblock_metrics::metrics::{
    SubscriptionCleanupOutcome, SubscriptionCleanupSource,
    inc_chainlink_subscription_cleanup_accounts,
};
use solana_pubkey::Pubkey;
use tokio::sync::mpsc;
use tracing::*;

use super::{
    ChainPubsubClient, SubscribedAccounts, SubscriptionKeyLocks,
    SubscriptionOwnershipMap, SubscriptionReason,
    subscription_key_owned_guard_from_map,
};
use crate::remote_account_provider::RemoteAccountProviderError;

/// Unsubscribes one account and records the cleanup outcome.
// NOTE: Pubkey stringification overhead is acceptable here since this is a cold path
// (network I/O dwarfs the stringification cost)
#[instrument(skip(pubsub_client), fields(pubkey = %pubkey))]
pub(crate) async fn unsubscribe_account<T: ChainPubsubClient>(
    pubkey: Pubkey,
    pubsub_client: &T,
    cleanup_source: SubscriptionCleanupSource,
) -> bool {
    match pubsub_client.unsubscribe(pubkey).await {
        Ok(()) => {
            inc_chainlink_subscription_cleanup_accounts(
                cleanup_source,
                SubscriptionCleanupOutcome::Unsubscribed,
            );
            true
        }
        Err(err) => {
            if matches!(
                err,
                RemoteAccountProviderError::AccountSubscriptionDoesNotExist(_)
            ) {
                debug!(error = ?err, "Failed to unsubscribe");
                inc_chainlink_subscription_cleanup_accounts(
                    cleanup_source,
                    SubscriptionCleanupOutcome::AlreadyAbsent,
                );
            } else {
                warn!(error = ?err, "Failed to unsubscribe");
                inc_chainlink_subscription_cleanup_accounts(
                    cleanup_source,
                    SubscriptionCleanupOutcome::UnsubscribeFailed,
                );
            }
            false
        }
    }
}

/// Reconciles subscription state between the subscription set and the pubsub client.
///
/// This function is called when a mismatch is detected between the accounts
/// tracked in the subscription set and the actual subscriptions held by the pubsub
/// client. It repairs drift by:
///
/// - **Resubscribing**: Accounts present in the subscription set but missing from the
///   pubsub client are resubscribed. This can happen if subscriptions were
///   dropped due to network issues or reconnects.
///
/// - **Unsubscribing**: Accounts present in the pubsub client but missing from
///   the subscription set are unsubscribed.
///
/// `internally_managed` contains system accounts, such as sysvars, that are
/// expected to be present in pubsub without being tracked in the subscription
/// set. These are excluded so reconciliation does not unsubscribe them.
///
/// When `subscription_key_locks` is provided, reconciliation serializes each
/// per-pubkey repair with normal subscription transitions and rechecks the live
/// subscription-set/pubsub state after acquiring the lock. This prevents a stale snapshot
/// from unsubscribing an account that is in the middle of being registered, while
/// still allowing tests to exercise the unlocked reconciliation path by passing
/// `None`.
///
/// Returns the expected total number of monitored accounts: tracked accounts
/// plus internally managed accounts.
pub(crate) async fn reconcile_subscriptions<PubsubClient: ChainPubsubClient>(
    subscribed_accounts: &SubscribedAccounts,
    pubsub_client: &PubsubClient,
    internally_managed: &[Pubkey],
    stale_account_tx: &mpsc::Sender<Pubkey>,
    subscription_key_locks: Option<&SubscriptionKeyLocks>,
    subscription_ownership: Option<&SubscriptionOwnershipMap>,
) -> usize {
    let tracked_pubkeys = subscribed_accounts.pubkeys();
    let tracked_count = tracked_pubkeys.len();

    let Some(pubsub_snapshot) =
        pubsub_client.subscription_reconciliation_snapshot()
    else {
        debug!(
            tracked_count = tracked_count,
            internally_managed_count = internally_managed.len(),
            "Skipping subscription reconciliation because no connected pubsub client is available"
        );
        return tracked_count + internally_managed.len();
    };

    // Internally managed keys (e.g. the clock sysvar) are outside the tracked
    // set, so collect missing ones for direct resubscription.
    let missing_internally_managed: Vec<Pubkey> = internally_managed
        .iter()
        .filter(|pk| !pubsub_snapshot.intersection.contains(pk))
        .copied()
        .collect();

    let ensured_subs_without_never_evict: HashSet<_> = pubsub_snapshot
        .intersection
        .into_iter()
        .filter(|pk| !internally_managed.contains(pk))
        .collect();
    let partial_subs_without_never_evict: HashSet<_> = pubsub_snapshot
        .union
        .into_iter()
        .filter(|pk| !internally_managed.contains(pk))
        .collect();

    // A) pubsub tracking subs that are not ensured by all clients
    let missing_in_pubsub: HashSet<_> = tracked_pubkeys
        .difference(&ensured_subs_without_never_evict)
        .collect();
    // B) Subs not in pubsub tracking that some clients are subscribed to
    let extra_in_pubsub: HashSet<_> = partial_subs_without_never_evict
        .difference(&tracked_pubkeys)
        .collect();

    trace!(
        tracked_count = tracked_count,
        ensured_count = ensured_subs_without_never_evict.len(),
        partial_count = partial_subs_without_never_evict.len(),
        missing_in_pubsub_count = missing_in_pubsub.len(),
        extra_in_pubsub_count = extra_in_pubsub.len(),
        "Reconciling subscriptions between pubsub tracking and pubsub client"
    );

    // Resubscribe internally managed keys missing from any client. Clients that
    // already hold the subscription dedup the call.
    if !missing_internally_managed.is_empty() {
        warn!(
            pubkeys = ?missing_internally_managed,
            "Resubscribing missing internally managed accounts"
        );
        for pubkey in missing_internally_managed {
            let _subscription_guard =
                acquire_subscription_key_guard(subscription_key_locks, pubkey)
                    .await;
            if let Err(e) = pubsub_client.subscribe(pubkey, None).await {
                warn!(
                    pubkey = %pubkey,
                    error = ?e,
                    "Failed to resubscribe internally managed account"
                );
            }
        }
    }

    // For any sub that is in the pubsub tracking but not ensured by all clients we resubscribe.
    // This may call subscribe on some clients that already have the subscription and
    // is ignored by that client.
    if !missing_in_pubsub.is_empty() {
        static LOG_TRACE_COUNT: AtomicU16 = AtomicU16::new(0);
        // If this happens a lot then this is serious since that means that some clients
        // were not subscribed to all accounts
        let len = missing_in_pubsub.len();
        let err = RemoteAccountProviderError::AccountSubscriptionsOutOfSync(
            format!("{len} accounts in pubsub tracking but not in pubsub"),
        );
        log_trace_warn(
            "Consolidating missing subscriptions",
            "Consolidated missing subscriptions repeatedly",
            &len.to_string(),
            &err,
            100,
            &LOG_TRACE_COUNT,
        );
        trace!(pubkeys = ?missing_in_pubsub, "Resubscribing missing accounts");
        for pubkey in missing_in_pubsub {
            let pubkey = *pubkey;
            let _subscription_guard =
                acquire_subscription_key_guard(subscription_key_locks, pubkey)
                    .await;

            if !subscribed_accounts.contains(&pubkey) {
                trace!(
                    pubkey = %pubkey,
                    "Skipping resubscribe because pubkey left pubsub tracking after reconciliation snapshot"
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
                continue;
            }

            // Undelegation tracking must stay watched until undelegation
            // completes; keep the entry and retry next cycle.
            if let Some(ownership) = subscription_ownership
                && ownership.lock().await.get(&pubkey).is_some_and(|own| {
                    own.contains(SubscriptionReason::UndelegationTracking)
                })
            {
                continue;
            }

            // No client holds the subscription: evict so the account is
            // re-cloned fresh on next use instead of going stale.
            warn!(
                pubkey = %pubkey,
                error = ?resub_err,
                "Resubscribe did not take effect on any client; evicting account to prevent stale state"
            );
            subscribed_accounts.remove(&pubkey);
            if let Some(ownership) = subscription_ownership {
                ownership.lock().await.remove(&pubkey);
            }
            if let Err(err) = stale_account_tx.send(pubkey).await {
                warn!(pubkey = %pubkey, error = ?err, "Failed to enqueue stale account after subscription loss");
                inc_chainlink_subscription_cleanup_accounts(
                    SubscriptionCleanupSource::Reconciler,
                    SubscriptionCleanupOutcome::RemovalUpdateFailed,
                );
            } else {
                inc_chainlink_subscription_cleanup_accounts(
                    SubscriptionCleanupSource::Reconciler,
                    SubscriptionCleanupOutcome::Unsubscribed,
                );
            }
        }
    }

    // Subscriptions held by a client but absent from tracking are stale remote
    // state. Reconciliation unsubscribes them without touching the bank;
    // engine cache eviction owns normal bank removal.
    // This may call unsubscribe on some clients that don't have the subscription and
    // is ignored by that client.
    if !extra_in_pubsub.is_empty() {
        debug!(
            count = extra_in_pubsub.len(),
            "Unsubscribing accounts in pubsub but not in pubsub tracking"
        );
        trace!(pubkeys = ?extra_in_pubsub, "Unsubscribing stale accounts");
        for pubkey in extra_in_pubsub {
            let pubkey = *pubkey;
            let _subscription_guard =
                acquire_subscription_key_guard(subscription_key_locks, pubkey)
                    .await;

            if subscribed_accounts.contains(&pubkey) {
                trace!(
                    pubkey = %pubkey,
                    "Skipping stale unsubscribe because pubkey entered pubsub tracking after reconciliation snapshot"
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

            unsubscribe_account(
                pubkey,
                pubsub_client,
                SubscriptionCleanupSource::Reconciler,
            )
            .await;
        }
    }
    // We assume that reconciling worked and now our subscribed accounts are up to date
    // Pubsubs should now contain all tracked and internally managed accounts.
    tracked_count + internally_managed.len()
}

async fn acquire_subscription_key_guard(
    subscription_key_locks: Option<&SubscriptionKeyLocks>,
    pubkey: Pubkey,
) -> Option<tokio::sync::OwnedMutexGuard<()>> {
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
    use std::sync::Arc;

    use solana_pubkey::Pubkey;
    use tokio::sync::mpsc;

    use super::*;
    use crate::{
        remote_account_provider::{
            chain_pubsub_client::mock::ChainPubsubClientMock,
            pubsub_common::SubscriptionUpdate,
            subscribed_accounts::SubscribedAccounts,
        },
        testing::init_logger,
    };

    fn create_test_pubkey(seed: u8) -> Pubkey {
        let mut bytes = [0u8; 32];
        bytes[0] = seed;
        Pubkey::from(bytes)
    }

    #[tokio::test]
    async fn test_subs_in_tracking_and_clients_same_noop() {
        init_logger();

        let (tx, rx) = mpsc::channel::<SubscriptionUpdate>(10);
        let mock_client = ChainPubsubClientMock::new(tx, rx);

        // Set up pubsub tracking with 2 accounts
        let subscriptions = SubscribedAccounts::default();
        let pk1 = create_test_pubkey(1);
        let pk2 = create_test_pubkey(2);

        subscriptions.add(pk1);
        subscriptions.add(pk2);

        // Set up client with same subscriptions
        mock_client.insert_subscription(pk1);
        mock_client.insert_subscription(pk2);

        // Create removal channel
        let (removed_tx, _removed_rx) = mpsc::channel::<Pubkey>(10);

        // Reconcile
        reconcile_subscriptions_local(
            &subscriptions,
            &mock_client,
            &[],
            &removed_tx,
        )
        .await;

        // Verify subscriptions are unchanged
        let subs = mock_client.subscriptions_union();
        assert_eq!(subs.len(), 2);
        assert!(subs.contains(&pk1));
        assert!(subs.contains(&pk2));
    }

    #[tokio::test]
    async fn test_not_all_tracked_subs_ensured_resubscribes() {
        init_logger();

        let (tx, rx) = mpsc::channel::<SubscriptionUpdate>(10);
        let mock_client = ChainPubsubClientMock::new(tx, rx);

        // Set up pubsub tracking with 3 accounts
        let subscriptions = SubscribedAccounts::default();
        let pk1 = create_test_pubkey(1);
        let pk2 = create_test_pubkey(2);
        let pk3 = create_test_pubkey(3);

        subscriptions.add(pk1);
        subscriptions.add(pk2);
        subscriptions.add(pk3);

        // Client only has pk1 and pk2
        mock_client.insert_subscription(pk1);
        mock_client.insert_subscription(pk2);

        // Create removal channel
        let (removed_tx, _removed_rx) = mpsc::channel::<Pubkey>(10);

        // Reconcile
        reconcile_subscriptions_local(
            &subscriptions,
            &mock_client,
            &[],
            &removed_tx,
        )
        .await;

        // Verify pk3 was resubscribed
        let subs = mock_client.subscriptions_union();
        assert_eq!(subs.len(), 3);
        assert!(subs.contains(&pk1));
        assert!(subs.contains(&pk2));
        assert!(subs.contains(&pk3));
    }

    #[tokio::test]
    async fn test_internally_managed_excluded() {
        init_logger();

        let (tx, rx) = mpsc::channel::<SubscriptionUpdate>(10);
        let mock_client = ChainPubsubClientMock::new(tx, rx);

        // Set up pubsub tracking with 2 accounts
        let subscriptions = SubscribedAccounts::default();
        let pk1 = create_test_pubkey(1);
        let pk2 = create_test_pubkey(2);
        let internally_managed_pk = create_test_pubkey(99);

        subscriptions.add(pk1);
        subscriptions.add(pk2);

        // Client has all 3 subscriptions
        mock_client.insert_subscription(pk1);
        mock_client.insert_subscription(pk2);
        mock_client.insert_subscription(internally_managed_pk);

        // Create removal channel
        let (removed_tx, mut removed_rx) = mpsc::channel::<Pubkey>(10);
        let internally_managed = vec![internally_managed_pk];

        // Reconcile
        reconcile_subscriptions_local(
            &subscriptions,
            &mock_client,
            &internally_managed,
            &removed_tx,
        )
        .await;

        // Verify internally_managed_pk is still subscribed
        let subs = mock_client.subscriptions_union();
        assert_eq!(subs.len(), 3);
        assert!(subs.contains(&pk1));
        assert!(subs.contains(&pk2));
        assert!(subs.contains(&internally_managed_pk));

        // Verify no removal notification for internally_managed_pk
        assert!(removed_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn test_resubscribe_missing_account() {
        init_logger();

        let (tx, rx) = mpsc::channel::<SubscriptionUpdate>(10);
        let mock_client = ChainPubsubClientMock::new(tx, rx);

        // Set up pubsub tracking with pk1, pk2, pk3
        let subscriptions = SubscribedAccounts::default();
        let pk1 = create_test_pubkey(1);
        let pk2 = create_test_pubkey(2);
        let pk3 = create_test_pubkey(3);

        subscriptions.add(pk1);
        subscriptions.add(pk2);
        subscriptions.add(pk3);

        // Client only has pk1 and pk2
        mock_client.insert_subscription(pk1);
        mock_client.insert_subscription(pk2);

        // Create removal channel
        let (removed_tx, _removed_rx) = mpsc::channel::<Pubkey>(10);

        // Reconcile
        reconcile_subscriptions_local(
            &subscriptions,
            &mock_client,
            &[],
            &removed_tx,
        )
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

        let subscriptions = SubscribedAccounts::default();
        let pk = create_test_pubkey(1);
        subscriptions.add(pk);

        let (removed_tx, mut removed_rx) = mpsc::channel::<Pubkey>(10);
        reconcile_subscriptions_local(
            &subscriptions,
            &mock_client,
            &[],
            &removed_tx,
        )
        .await;

        assert!(mock_client.subscriptions_union().contains(&pk));
        assert!(subscriptions.contains(&pk));
        assert!(removed_rx.try_recv().is_err());
    }

    /// When no client holds the subscription after the resubscribe, the
    /// account must be evicted instead of being left stale in the bank.
    #[tokio::test]
    async fn test_silently_dead_subscription_evicted_when_repair_fails() {
        init_logger();

        let (tx, rx) = mpsc::channel::<SubscriptionUpdate>(10);
        let mock_client = ChainPubsubClientMock::new(tx, rx);

        let subscriptions = SubscribedAccounts::default();
        let pk = create_test_pubkey(1);
        subscriptions.add(pk);

        mock_client.silently_noop_next_subscriptions(1);

        let (removed_tx, mut removed_rx) = mpsc::channel::<Pubkey>(10);
        reconcile_subscriptions_local(
            &subscriptions,
            &mock_client,
            &[],
            &removed_tx,
        )
        .await;

        assert!(!mock_client.subscriptions_union().contains(&pk));
        assert!(!subscriptions.contains(&pk));
        assert_eq!(removed_rx.try_recv(), Ok(pk));
    }

    /// Accounts owned for undelegation tracking must never be evicted; they
    /// are kept and retried on the next cycle.
    #[tokio::test]
    async fn test_undelegation_tracked_subscription_is_not_evicted() {
        init_logger();

        let (tx, rx) = mpsc::channel::<SubscriptionUpdate>(10);
        let mock_client = ChainPubsubClientMock::new(tx, rx);

        let subscriptions = SubscribedAccounts::default();
        let pk = create_test_pubkey(1);
        subscriptions.add(pk);

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
            &subscriptions,
            &mock_client,
            &[],
            &removed_tx,
            None,
            Some(&ownership),
        )
        .await;

        assert!(subscriptions.contains(&pk));
        assert!(removed_rx.try_recv().is_err());
    }

    /// Test case: Empty pubsub tracking should cause resubscribe of all pubsub tracking accounts if missing
    /// Expected: No-op if pubsub is also empty (single client case)
    #[tokio::test]
    async fn test_empty_tracking_and_pubsub_is_noop() {
        init_logger();

        let (tx, rx) = mpsc::channel::<SubscriptionUpdate>(10);
        let mock_client = ChainPubsubClientMock::new(tx, rx);

        // Set up empty pubsub tracking
        let subscriptions = SubscribedAccounts::default();

        // Empty pubsub (single client case)
        // Create removal channel
        let (removed_tx, _removed_rx) = mpsc::channel::<Pubkey>(10);

        // Reconcile
        reconcile_subscriptions_local(
            &subscriptions,
            &mock_client,
            &[],
            &removed_tx,
        )
        .await;

        // Verify state unchanged (both empty)
        let subs = mock_client.subscriptions_union();
        assert_eq!(subs.len(), 0);
    }

    /// Test case: Empty pubsub with subscriptions in pubsub tracking
    /// Expected: Resubscribe to all accounts
    #[tokio::test]
    async fn test_empty_pubsub_resubscribes_all() {
        init_logger();

        let (tx, rx) = mpsc::channel::<SubscriptionUpdate>(10);
        let mock_client = ChainPubsubClientMock::new(tx, rx);

        // Set up pubsub tracking with 2 accounts
        let subscriptions = SubscribedAccounts::default();
        let pk1 = create_test_pubkey(1);
        let pk2 = create_test_pubkey(2);
        subscriptions.add(pk1);
        subscriptions.add(pk2);

        // Empty pubsub

        // Create removal channel
        let (removed_tx, _removed_rx) = mpsc::channel::<Pubkey>(10);

        // Reconcile
        reconcile_subscriptions_local(
            &subscriptions,
            &mock_client,
            &[],
            &removed_tx,
        )
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

        // Set up pubsub tracking with pk1, pk2, pk3, pk4
        let subscriptions = SubscribedAccounts::default();
        let pk1 = create_test_pubkey(1);
        let pk2 = create_test_pubkey(2);
        let pk3 = create_test_pubkey(3);
        let pk4 = create_test_pubkey(4);

        subscriptions.add(pk1);
        subscriptions.add(pk2);
        subscriptions.add(pk3);
        subscriptions.add(pk4);

        // Client only has pk1 and pk2
        mock_client.insert_subscription(pk1);
        mock_client.insert_subscription(pk2);

        // Create removal channel
        let (removed_tx, _removed_rx) = mpsc::channel::<Pubkey>(10);

        // Reconcile
        reconcile_subscriptions_local(
            &subscriptions,
            &mock_client,
            &[],
            &removed_tx,
        )
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

        let subscribed_accounts = Arc::new(SubscribedAccounts::default());

        let pubkey1 = create_test_pubkey(1);
        let pubkey2 = create_test_pubkey(2);
        let pubkey3 = create_test_pubkey(3);

        // Add accounts to subscription set
        subscribed_accounts.add(pubkey1);
        subscribed_accounts.add(pubkey2);
        subscribed_accounts.add(pubkey3);

        // Only pubkey1 is in pubsub (simulating missing subscriptions)
        pubsub_client.insert_subscription(pubkey1);

        let internally_managed: Vec<Pubkey> = vec![];

        // Reconcile should resubscribe pubkey2 and pubkey3
        reconcile_subscriptions_local(
            &subscribed_accounts,
            &pubsub_client,
            &internally_managed,
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
    async fn test_reconcile_unsubscribes_accounts_not_in_tracking() {
        init_logger();

        let (tx, rx) = mpsc::channel(1);
        let pubsub_client = ChainPubsubClientMock::new(tx, rx);
        let (removed_tx, mut removed_rx) = mpsc::channel(10);

        let subscribed_accounts = Arc::new(SubscribedAccounts::default());

        let pubkey1 = create_test_pubkey(1);
        let pubkey2 = create_test_pubkey(2);
        let pubkey3 = create_test_pubkey(3);

        // Only pubkey1 is in subscription set
        subscribed_accounts.add(pubkey1);

        // All three are in pubsub (simulating stale subscriptions)
        pubsub_client.insert_subscription(pubkey1);
        pubsub_client.insert_subscription(pubkey2);
        pubsub_client.insert_subscription(pubkey3);

        let internally_managed: Vec<Pubkey> = vec![];

        // Reconcile should unsubscribe pubkey2 and pubkey3
        reconcile_subscriptions_local(
            &subscribed_accounts,
            &pubsub_client,
            &internally_managed,
            &removed_tx,
        )
        .await;

        // Verify only pubkey1 remains subscribed
        let subs = pubsub_client.subscriptions_union();
        assert!(subs.contains(&pubkey1));
        assert!(!subs.contains(&pubkey2));
        assert!(!subs.contains(&pubkey3));
        assert_eq!(subs.len(), 1);

        // Reconciliation repairs remote state only. Account removal is driven
        // by engine cache eviction.
        let removed = drain_removed_account_rx(&mut removed_rx);
        assert!(removed.is_empty());
    }

    #[tokio::test]
    async fn test_reconcile_preserves_internally_managed_not_in_tracking() {
        init_logger();

        let (tx, rx) = mpsc::channel(1);
        let pubsub_client = ChainPubsubClientMock::new(tx, rx);
        let (removed_tx, mut removed_rx) = mpsc::channel(10);

        let subscribed_accounts = Arc::new(SubscribedAccounts::default());

        let tracked_pubkey = create_test_pubkey(1);
        let internally_managed_pubkey = create_test_pubkey(2);
        let stale_pubkey = create_test_pubkey(3);

        // Only tracked_pubkey is in subscription set (internally_managed_pubkey is NOT in pubsub tracking)
        subscribed_accounts.add(tracked_pubkey);

        // All three are subscribed in pubsub
        pubsub_client.insert_subscription(tracked_pubkey);
        pubsub_client.insert_subscription(internally_managed_pubkey);
        pubsub_client.insert_subscription(stale_pubkey);

        // internally_managed_pubkey is marked as internally_managed, so it should be
        // preserved even though it's not in the subscription set
        let internally_managed = vec![internally_managed_pubkey];

        reconcile_subscriptions_local(
            &subscribed_accounts,
            &pubsub_client,
            &internally_managed,
            &removed_tx,
        )
        .await;

        // Verify: tracked_pubkey and internally_managed_pubkey remain, stale_pubkey
        // is unsubscribed
        let subs = pubsub_client.subscriptions_union();
        assert!(
            subs.contains(&tracked_pubkey),
            "Account in pubsub tracking should remain subscribed"
        );
        assert!(
            subs.contains(&internally_managed_pubkey),
            "Never-evicted account should remain subscribed even if not in pubsub tracking"
        );
        assert!(
            !subs.contains(&stale_pubkey),
            "Stale account not in pubsub tracking and not never-evicted should be \
             unsubscribed"
        );
        assert_eq!(subs.len(), 2);

        // Reconciliation repairs remote state only. Account removal is driven
        // by engine cache eviction.
        let removed = drain_removed_account_rx(&mut removed_rx);
        assert!(removed.is_empty());
    }

    async fn reconcile_subscriptions_local<PubsubClient: ChainPubsubClient>(
        subscribed_accounts: &SubscribedAccounts,
        pubsub_client: &PubsubClient,
        internally_managed: &[Pubkey],
        removed_account_tx: &mpsc::Sender<Pubkey>,
    ) -> usize {
        reconcile_subscriptions(
            subscribed_accounts,
            pubsub_client,
            internally_managed,
            removed_account_tx,
            None,
            None,
        )
        .await
    }
}
