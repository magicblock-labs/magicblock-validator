use std::collections::HashSet;

use solana_pubkey::Pubkey;
use tokio::sync::mpsc;
use tracing::*;

use super::{AccountsLruCache, ChainPubsubClient};

/// Unsubscribes from pubsub and sends a removal notification to trigger bank
/// removal.
///
/// This is the core logic shared between:
/// - Normal unsubscribe flow (after removing from LRU cache)
/// - Reconciliation flow (account missing from LRU cache)
#[instrument(skip(pubsub_client, removed_account_tx), fields(pubkey = %pubkey))]
pub(crate) async fn unsubscribe_and_notify_removal<T: ChainPubsubClient>(
    pubkey: Pubkey,
    pubsub_client: &T,
    removed_account_tx: &mpsc::Sender<Pubkey>,
) -> bool {
    match pubsub_client.unsubscribe(pubkey).await {
        Ok(()) => {
            if let Err(err) = removed_account_tx.send(pubkey).await {
                warn!(error = ?err, "Failed to send removal update");
            }
            true
        }
        Err(err) => {
            warn!(error = ?err, "Failed to unsubscribe");
            false
        }
    }
}

/// Reconciles subscription state between the LRU cache and the pubsub client.
///
/// This function is called when a mismatch is detected between the accounts
/// tracked in the LRU cache and the actual subscriptions held by the pubsub
/// client. It ensures both are in sync by:
///
/// - **Resubscribing**: Accounts present in the LRU cache but missing from the
///   pubsub client are resubscribed. This can happen if subscriptions were
///   dropped due to network issues or reconnections.
///
/// - **Unsubscribing**: Accounts present in the pubsub client but missing from
///   the LRU cache are unsubscribed. This can happen if the LRU evicted an
///   account but the unsubscribe request failed or was lost.
///
/// # Parameters
///
/// - `subscribed_accounts`: The LRU cache that tracks which accounts should be
///   subscribed. This is the source of truth for user-requested subscriptions.
///
/// - `pubsub_client`: The client managing actual WebSocket/gRPC subscriptions
///   to the chain. Provides the current set of active subscriptions.
///
/// - `never_evicted`: A list of system accounts (e.g., sysvar::clock) that are
///   always subscribed but are **not** tracked in the LRU cache. These accounts
///   are excluded from reconciliation because:
///   1. They are subscribed directly without going through the LRU.
///   2. They should never be unsubscribed regardless of LRU state.
///   3. Their presence in pubsub but absence from LRU is expected and correct.
///
///   Without filtering these out, the reconciler would incorrectly attempt to
///   unsubscribe them since they appear in pubsub but not in the LRU cache.
///
/// - `removed_account_tx`: Channel to notify upstream that an account was
///   unsubscribed and should be removed from the bank.
pub async fn reconcile_subscriptions<PubsubClient: ChainPubsubClient>(
    subscribed_accounts: &AccountsLruCache,
    pubsub_client: &PubsubClient,
    never_evicted: &[Pubkey],
    removed_account_tx: &mpsc::Sender<Pubkey>,
) {
    let pubsub_union = pubsub_client.subscriptions_union();
    let pubsub_intersection = pubsub_client.subscriptions_intersection();
    let lru_pubkeys = subscribed_accounts.pubkeys();

    let ensured_subs_without_never_evict: HashSet<_> = pubsub_intersection
        .into_iter()
        .filter(|pk| !never_evicted.contains(pk))
        .collect();
    let partial_subs_without_never_evict: HashSet<_> = pubsub_union
        .into_iter()
        .filter(|pk| !never_evicted.contains(pk))
        .collect();
    let lru_pubkeys_set: HashSet<_> = lru_pubkeys.into_iter().collect();

    // A) LRU subs that are not ensured by all clients
    let extra_in_lru: Vec<_> = lru_pubkeys_set
        .difference(&ensured_subs_without_never_evict)
        .collect();
    // B) Subs not in LRU that some clients are subscribed to
    let extra_in_pubsub: Vec<_> = partial_subs_without_never_evict
        .difference(&ensured_subs_without_never_evict)
        .collect();

    // For any sub that is in the LRU but not ensured by all clients we resubscribe.
    // This may call subscribe on some clients that already have the subscription and
    // is ignored by that client.
    if !extra_in_lru.is_empty() {
        debug!(
            count = extra_in_lru.len(),
            "Resubscribing accounts in LRU but not in pubsub"
        );
        for pubkey in extra_in_lru {
            if let Err(e) = pubsub_client.subscribe(*pubkey, None).await {
                warn!(pubkey = %pubkey, error = ?e, "Failed to resubscribe account");
            }
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
        for pubkey in extra_in_pubsub {
            unsubscribe_and_notify_removal(
                *pubkey,
                pubsub_client,
                removed_account_tx,
            )
            .await;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroUsize;
    use std::sync::Arc;

    use solana_pubkey::Pubkey;
    use tokio::sync::mpsc;

    use crate::{
        remote_account_provider::{
            chain_pubsub_client::mock::ChainPubsubClientMock,
            lru_cache::AccountsLruCache, pubsub_common::SubscriptionUpdate,
        },
        testing::init_logger,
    };

    use super::*;

    fn create_test_pubkey(seed: u8) -> Pubkey {
        let mut bytes = [0u8; 32];
        bytes[0] = seed;
        Pubkey::from(bytes)
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
        reconcile_subscriptions(&lru, &mock_client, &[], &removed_tx).await;

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
        reconcile_subscriptions(&lru, &mock_client, &[], &removed_tx).await;

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
        reconcile_subscriptions(
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
        reconcile_subscriptions(&lru, &mock_client, &[], &removed_tx).await;

        // Verify pk3 was resubscribed
        let subs = mock_client.subscriptions_union();
        assert_eq!(subs.len(), 3);
        assert!(subs.contains(&pk1));
        assert!(subs.contains(&pk2));
        assert!(subs.contains(&pk3));
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
        reconcile_subscriptions(&lru, &mock_client, &[], &removed_tx).await;

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
        reconcile_subscriptions(&lru, &mock_client, &[], &removed_tx).await;

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
        reconcile_subscriptions(&lru, &mock_client, &[], &removed_tx).await;

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

        let pubkey1 = Pubkey::new_unique();
        let pubkey2 = Pubkey::new_unique();
        let pubkey3 = Pubkey::new_unique();

        // Add accounts to LRU cache
        lru_cache.add(pubkey1);
        lru_cache.add(pubkey2);
        lru_cache.add(pubkey3);

        // Only pubkey1 is in pubsub (simulating missing subscriptions)
        pubsub_client.insert_subscription(pubkey1);

        let never_evicted: Vec<Pubkey> = vec![];

        // Reconcile should resubscribe pubkey2 and pubkey3
        reconcile_subscriptions(
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

        let pubkey1 = Pubkey::new_unique();
        let pubkey2 = Pubkey::new_unique();
        let pubkey3 = Pubkey::new_unique();

        // Only pubkey1 is in LRU cache
        lru_cache.add(pubkey1);

        // All three are in pubsub (simulating stale subscriptions)
        pubsub_client.insert_subscription(pubkey1);
        pubsub_client.insert_subscription(pubkey2);
        pubsub_client.insert_subscription(pubkey3);

        let never_evicted: Vec<Pubkey> = vec![];

        // Reconcile should unsubscribe pubkey2 and pubkey3
        reconcile_subscriptions(
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

        let pubkey_in_lru = Pubkey::new_unique();
        let never_evicted_pubkey = Pubkey::new_unique();
        let stale_pubkey = Pubkey::new_unique();

        // Only pubkey_in_lru is in LRU cache (never_evicted_pubkey is NOT in LRU)
        lru_cache.add(pubkey_in_lru);

        // All three are subscribed in pubsub
        pubsub_client.insert_subscription(pubkey_in_lru);
        pubsub_client.insert_subscription(never_evicted_pubkey);
        pubsub_client.insert_subscription(stale_pubkey);

        // never_evicted_pubkey is marked as never_evicted, so it should be
        // preserved even though it's not in the LRU cache
        let never_evicted = vec![never_evicted_pubkey];

        reconcile_subscriptions(
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
}
