use std::{
    num::NonZeroUsize,
    path::{Path, PathBuf},
    sync::{atomic::AtomicU64, Arc},
    time::Duration,
};

use magicblock_metrics::metrics::{
    chainlink_companion_fetch_attempts_sample_count,
    chainlink_companion_fetch_attempts_sample_sum,
    chainlink_companion_fetch_duration_sample_count,
    chainlink_companion_fetch_duration_sample_sum,
    chainlink_pending_fetch_accounts_value,
    chainlink_pending_fetch_waiters_gauge_value,
    chainlink_pending_fetch_waiters_value,
    chainlink_subscription_cleanup_accounts_value,
    chainlink_subscription_registration_accounts_value,
    chainlink_subscription_release_accounts_value, AccountFetchReason,
    ChainlinkCompanionFetchKind, ChainlinkCompanionFetchOutcome,
    ChainlinkPendingFetchLayer, ChainlinkPendingFetchOutcome,
};
use solana_account::Account;
use solana_system_interface::program as system_program;
use tokio::sync::{mpsc, oneshot};

use super::*;
use crate::{
    remote_account_provider::{
        chain_pubsub_client::mock::ChainPubsubClientMock, chain_slot::ChainSlot,
    },
    testing::{
        init_logger,
        rpc_client_mock::{
            AccountAtSlot, ChainRpcClientMock, ChainRpcClientMockBuilder,
        },
        utils::{create_test_lru_cache, random_pubkey},
    },
};

struct ProviderTestCtx {
    provider:
        Arc<RemoteAccountProvider<ChainRpcClientMock, ChainPubsubClientMock>>,
    rpc_client: ChainRpcClientMock,
    pubsub_client: ChainPubsubClientMock,
    _forward_rx: mpsc::Receiver<ForwardedSubscriptionUpdate>,
}

struct MultiplexedProviderTestCtx {
    provider: Arc<
        RemoteAccountProvider<
            ChainRpcClientMock,
            SubMuxClient<ChainPubsubClientMock>,
        >,
    >,
    rpc_client: ChainRpcClientMock,
    websocket_client: Arc<ChainPubsubClientMock>,
    grpc_client: Arc<ChainPubsubClientMock>,
    _abort_senders: Vec<mpsc::Sender<()>>,
    _forward_rx: mpsc::Receiver<ForwardedSubscriptionUpdate>,
}

async fn setup_provider(
    pubkey: solana_pubkey::Pubkey,
    account: Account,
) -> ProviderTestCtx {
    setup_provider_with_lru_capacity(pubkey, account, 1000).await
}

async fn setup_provider_with_lru_capacity(
    pubkey: solana_pubkey::Pubkey,
    account: Account,
    lru_capacity: usize,
) -> ProviderTestCtx {
    let rpc_client = ChainRpcClientMockBuilder::new()
        .slot(100)
        .clock_sysvar_for_slot(100)
        .accounts(vec![(pubkey, account)].into_iter().collect())
        .build();

    let (updates_sender, updates_receiver) = mpsc::channel(1_000);
    let pubsub_client =
        ChainPubsubClientMock::new(updates_sender, updates_receiver);

    let (forward_tx, forward_rx) = mpsc::channel(1_000);
    let (subscribed_accounts, config) = create_test_lru_cache(lru_capacity);
    let config = config
        .with_secondary_subscriptions_lru_capacity(lru_capacity)
        .unwrap();
    let chain_slot = Arc::<AtomicU64>::default();

    let provider = Arc::new(
        RemoteAccountProvider::new(
            rpc_client.clone(),
            pubsub_client.clone(),
            forward_tx,
            &config,
            subscribed_accounts,
            ChainSlot::new(chain_slot),
        )
        .await
        .unwrap(),
    );

    ProviderTestCtx {
        provider,
        rpc_client,
        pubsub_client,
        _forward_rx: forward_rx,
    }
}

async fn setup_multiplexed_provider(
    pubkey: Pubkey,
    account: Account,
) -> MultiplexedProviderTestCtx {
    let rpc_client = ChainRpcClientMockBuilder::new()
        .slot(100)
        .clock_sysvar_for_slot(100)
        .accounts(vec![(pubkey, account)].into_iter().collect())
        .build();

    let (ws_updates_sender, ws_updates_receiver) = mpsc::channel(1_000);
    let websocket_client = Arc::new(ChainPubsubClientMock::new(
        ws_updates_sender,
        ws_updates_receiver,
    ));
    let (grpc_updates_sender, grpc_updates_receiver) = mpsc::channel(1_000);
    let grpc_client = Arc::new(ChainPubsubClientMock::new(
        grpc_updates_sender,
        grpc_updates_receiver,
    ));
    grpc_client.set_transport(PubsubTransport::Grpc);

    let (ws_abort_sender, ws_abort_receiver) = mpsc::channel(1);
    let (grpc_abort_sender, grpc_abort_receiver) = mpsc::channel(1);
    let (subscribed_accounts, config) = create_test_lru_cache(1_000);
    let secondary_subscriptions =
        Arc::new(AccountsLruCache::new(NonZeroUsize::new(1_000).unwrap()));
    let tracker = Arc::new(TieredSubscribedAccountsTracker::new(
        subscribed_accounts.clone(),
        secondary_subscriptions.clone(),
    ));
    let pubsub_client = SubMuxClient::new(
        vec![
            (websocket_client.clone(), ws_abort_receiver),
            (grpc_client.clone(), grpc_abort_receiver),
        ],
        tracker,
        None,
    );
    let (forward_tx, forward_rx) = mpsc::channel(1_000);
    let provider = Arc::new(
        RemoteAccountProvider::new_with_secondary_subscriptions(
            rpc_client.clone(),
            pubsub_client,
            forward_tx,
            &config,
            subscribed_accounts,
            secondary_subscriptions,
            ChainSlot::new(Arc::<AtomicU64>::default()),
        )
        .await
        .unwrap(),
    );

    MultiplexedProviderTestCtx {
        provider,
        rpc_client,
        websocket_client,
        grpc_client,
        _abort_senders: vec![ws_abort_sender, grpc_abort_sender],
        _forward_rx: forward_rx,
    }
}

fn pending_accounts_value(
    origin: impl Into<AccountFetchContext>,
    outcome: ChainlinkPendingFetchOutcome,
) -> u64 {
    chainlink_pending_fetch_accounts_value(
        origin,
        ChainlinkPendingFetchLayer::RemoteAccountProvider,
        outcome,
    )
}

fn pending_waiters_value(origin: impl Into<AccountFetchContext>) -> u64 {
    chainlink_pending_fetch_waiters_value(
        origin,
        ChainlinkPendingFetchLayer::RemoteAccountProvider,
    )
}

fn pending_waiters_gauge_value() -> i64 {
    chainlink_pending_fetch_waiters_gauge_value(
        ChainlinkPendingFetchLayer::RemoteAccountProvider,
    )
}

async fn wait_for_fetching_waiter_count(
    provider: &RemoteAccountProvider<ChainRpcClientMock, ChainPubsubClientMock>,
    pubkey: Pubkey,
    expected: usize,
) {
    let start = tokio::time::Instant::now();
    let timeout = Duration::from_secs(2);
    loop {
        let waiter_count = {
            let fetching = provider.fetching_accounts.lock().unwrap();
            fetching.get(&pubkey).map(|s| s.waiters.len()).unwrap_or(0)
        };
        if waiter_count == expected {
            break;
        }
        assert!(
            start.elapsed() < timeout,
            "fetching_accounts waiter count for {pubkey} should be \
             {expected} within {timeout:?}; got {waiter_count}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

async fn wait_for_direct_subscription(
    pubsub_client: &ChainPubsubClientMock,
    pubkey: Pubkey,
) {
    let start = tokio::time::Instant::now();
    let timeout = Duration::from_secs(2);
    loop {
        if pubsub_client.subscriptions_union().contains(&pubkey) {
            break;
        }
        assert!(
            start.elapsed() < timeout,
            "direct subscription for {pubkey} should be registered within {timeout:?}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

async fn wait_for_pending_account_delta_at_least(
    origin: impl Into<AccountFetchContext> + Copy,
    outcome: ChainlinkPendingFetchOutcome,
    baseline: u64,
    minimum_delta: u64,
) {
    let start = tokio::time::Instant::now();
    let timeout = Duration::from_secs(2);
    loop {
        let delta =
            pending_accounts_value(origin, outcome).saturating_sub(baseline);
        if delta >= minimum_delta {
            break;
        }
        assert!(
            start.elapsed() < timeout,
            "pending account metric delta for {outcome} should increase by at least \
             {minimum_delta} within {timeout:?}; got {delta}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

async fn wait_until(what: &str, condition: impl Fn() -> bool) {
    let start = tokio::time::Instant::now();
    let timeout = Duration::from_secs(2);
    loop {
        if condition() {
            break;
        }
        assert!(start.elapsed() < timeout, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// Waits for the detached evicted-cleanup task to drop the key's ownership
/// entry.
async fn wait_until_ownership_removed<T, U>(
    provider: &RemoteAccountProvider<T, U>,
    pubkey: &Pubkey,
) where
    T: crate::remote_account_provider::ChainRpcClient,
    U: ChainPubsubClient,
{
    let start = tokio::time::Instant::now();
    let timeout = Duration::from_secs(2);
    loop {
        if !provider
            .subscription_ownership
            .lock()
            .await
            .contains_key(pubkey)
        {
            break;
        }
        assert!(
            start.elapsed() < timeout,
            "timed out waiting for ownership removal of {pubkey}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test(flavor = "current_thread")]
async fn test_subscription_key_registry_cleans_cancelled_waiter() {
    let registry = Arc::new(SubscriptionKeyLockRegistry::default());
    let pubkey = Pubkey::new_unique();
    let owner = registry.acquire(pubkey).await;
    let waiter_registry = registry.clone();
    let waiter =
        tokio::spawn(async move { waiter_registry.acquire(pubkey).await });

    tokio::time::timeout(Duration::from_secs(1), async {
        while registry.registrations(&pubkey) != 2 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("waiter should register for the held key");

    // On the current-thread runtime the waiter cannot run between these two
    // statements: the owner observes a registered waiter, then cancellation
    // must remove the last registry entry.
    drop(owner);
    waiter.abort();
    let join = waiter.await;
    assert!(matches!(join, Err(err) if err.is_cancelled()));
    assert_eq!(registry.len(), 0);
}

/// Accounts that do not exist on chain stay in the secondary LRU and move to
/// the primary LRU once they are created.
#[tokio::test]
async fn test_not_found_account_stays_secondary_and_promotes_on_creation() {
    init_logger();

    let existing = random_pubkey();
    let missing = random_pubkey();
    let ctx = setup_provider(
        existing,
        Account {
            lamports: 500,
            ..Default::default()
        },
    )
    .await;
    ctx.pubsub_client.set_transport(PubsubTransport::Grpc);

    let res = ctx
        .provider
        .try_get(missing, AccountFetchContext::rpc_get_account())
        .await
        .unwrap();
    assert!(!res.is_found());
    let fetch_slot = res.slot();

    // The account remains in the secondary tier after the fetch resolves.
    let provider = ctx.provider.clone();
    wait_until("account to enter secondary tier", || {
        provider.secondary_subscriptions.contains(&missing)
    })
    .await;
    assert!(!ctx.provider.lrucache_subscribed_accounts.contains(&missing));
    assert!(ctx.provider.is_watching(&missing));
    assert!(ctx.pubsub_client.subscriptions_union().contains(&missing));

    // An older update must not undo the newer not-found classification.
    let transition_guard =
        ctx.provider.subscription_transition_lock.lock().await;
    let updates_before = ctx.provider.received_updates_count();
    ctx.pubsub_client
        .send_account_update(
            missing,
            fetch_slot.saturating_sub(1),
            &Account {
                lamports: 900,
                ..Default::default()
            },
        )
        .await;
    let provider = ctx.provider.clone();
    wait_until("older subscription update to be processed", || {
        provider.received_updates_count() > updates_before
    })
    .await;
    drop(transition_guard);
    let transition_guard =
        ctx.provider.subscription_transition_lock.lock().await;
    drop(transition_guard);
    assert!(ctx.provider.secondary_subscriptions.contains(&missing));
    assert!(!ctx.provider.lrucache_subscribed_accounts.contains(&missing));

    // A newer creation update promotes the account to the primary LRU.
    ctx.pubsub_client
        .send_account_update(
            missing,
            fetch_slot + 1,
            &Account {
                lamports: 1_000,
                ..Default::default()
            },
        )
        .await;
    let provider = ctx.provider.clone();
    wait_until("account to be promoted", || {
        provider.lrucache_subscribed_accounts.contains(&missing)
            && !provider.secondary_subscriptions.contains(&missing)
    })
    .await;
}

#[tokio::test]
async fn test_subscription_creation_fails_when_primary_capacity_is_protected() {
    init_logger();

    let protected = random_pubkey();
    let missing = random_pubkey();
    let mut ctx = setup_provider_with_lru_capacity(
        protected,
        Account {
            lamports: 1,
            ..Default::default()
        },
        1,
    )
    .await;
    ctx.pubsub_client.set_transport(PubsubTransport::Grpc);
    let mut removed_rx = ctx.provider.try_get_removed_account_rx().unwrap();

    ctx.provider
        .try_get(protected, AccountFetchContext::rpc_get_account())
        .await
        .unwrap();
    ctx.provider
        .acquire_subscription(
            &protected,
            SubscriptionReason::UndelegationTracking,
        )
        .await
        .unwrap();
    let fetch_slot = ctx
        .provider
        .try_get(missing, AccountFetchContext::rpc_get_account())
        .await
        .unwrap()
        .slot();

    let updates_before = ctx.provider.received_updates_count();
    ctx.pubsub_client
        .send_account_update(
            missing,
            fetch_slot + 1,
            &Account {
                lamports: 1,
                ..Default::default()
            },
        )
        .await;
    let provider = ctx.provider.clone();
    wait_until("subscription update to be rejected", || {
        provider.received_updates_count() > updates_before
    })
    .await;
    let transition_guard =
        ctx.provider.subscription_transition_lock.lock().await;
    drop(transition_guard);

    assert!(ctx
        .provider
        .lrucache_subscribed_accounts
        .contains(&protected));
    assert!(!ctx.provider.secondary_subscriptions.contains(&missing));
    assert!(!ctx.provider.is_watching(&missing));
    assert!(!ctx.pubsub_client.subscriptions_union().contains(&missing));
    assert!(ctx._forward_rx.try_recv().is_err());
    // The rejected promotion dropped the last watch; the removal pipeline
    // must be notified so a stale empty placeholder cannot outlive it.
    assert_eq!(wait_for_removed_account(&mut removed_rx).await, missing);
}

/// Models the update-pump race where a subscription update resolves a pending
/// fetch before the fetch's subscription setup created any tier state.
fn insert_pending_fetch(
    provider: &RemoteAccountProvider<ChainRpcClientMock, ChainPubsubClientMock>,
    pubkey: Pubkey,
    fetch_start_slot: u64,
) -> oneshot::Receiver<FetchResult> {
    let (waiter_tx, waiter_rx) = oneshot::channel();
    provider.fetching_accounts.lock().unwrap().insert(
        pubkey,
        FetchingAccountState {
            generation: provider.next_fetching_account_generation(),
            fetch_start_slot,
            fetch_context: AccountFetchContext::rpc_get_account(),
            requires_full_coverage: Arc::new(
                PendingFetchCoverageRequirement::new(false),
            ),
            owner_started_at: std::time::Instant::now(),
            waiters: vec![waiter_tx],
        },
    );
    waiter_rx
}

#[tokio::test]
async fn test_subscription_resolving_pending_fetch_without_tier_state_admits_to_primary(
) {
    init_logger();

    let existing = random_pubkey();
    let pending = random_pubkey();
    let ctx = setup_provider(existing, Account::default()).await;
    ctx.pubsub_client.set_transport(PubsubTransport::Grpc);

    let waiter_rx = insert_pending_fetch(&ctx.provider, pending, 100);
    // Deliver the update as a transport-level subscription (e.g. a gRPC
    // program subscription) without any provider tier state for the key.
    ctx.pubsub_client.insert_subscription(pending);
    ctx.pubsub_client
        .send_account_update(
            pending,
            101,
            &Account {
                lamports: 1,
                ..Default::default()
            },
        )
        .await;

    let resolved = tokio::time::timeout(Duration::from_secs(2), waiter_rx)
        .await
        .expect("timed out waiting for fetch resolution")
        .expect("waiter channel closed")
        .expect("subscription-resolved fetch should succeed");
    assert!(resolved.is_found());
    // The found account was admitted straight into the primary tier with an
    // active subscription; the in-flight setup adopts this membership.
    assert!(ctx.provider.lrucache_subscribed_accounts.contains(&pending));
    assert!(!ctx.provider.secondary_subscriptions.contains(&pending));
    assert!(ctx.pubsub_client.subscriptions_union().contains(&pending));
}

#[tokio::test]
async fn test_subscription_resolving_pending_fetch_without_tier_state_rejects_without_capacity(
) {
    init_logger();

    let protected = random_pubkey();
    let pending = random_pubkey();
    let ctx = setup_provider_with_lru_capacity(
        protected,
        Account {
            lamports: 1,
            ..Default::default()
        },
        1,
    )
    .await;
    ctx.pubsub_client.set_transport(PubsubTransport::Grpc);
    ctx.provider
        .acquire_subscription(
            &protected,
            SubscriptionReason::UndelegationTracking,
        )
        .await
        .unwrap();

    let waiter_rx = insert_pending_fetch(&ctx.provider, pending, 100);
    // Deliver the update as a transport-level subscription (e.g. a gRPC
    // program subscription) without any provider tier state for the key.
    ctx.pubsub_client.insert_subscription(pending);
    ctx.pubsub_client
        .send_account_update(
            pending,
            101,
            &Account {
                lamports: 1,
                ..Default::default()
            },
        )
        .await;

    // Found results must not reach waiters without primary admission; the
    // capacity rejection surfaces instead of the account.
    let err = tokio::time::timeout(Duration::from_secs(2), waiter_rx)
        .await
        .expect("timed out waiting for fetch resolution")
        .expect("waiter channel closed")
        .expect_err("found account without primary capacity must be rejected");
    assert!(matches!(
        err,
        RemoteAccountProviderError::AccountResolutionsFailed(message)
            if message.contains("No evictable subscription capacity")
                && message.contains(&pending.to_string())
    ));
    assert!(ctx
        .provider
        .lrucache_subscribed_accounts
        .contains(&protected));
    assert!(!ctx.provider.is_watching(&pending));
    // The rejection dropped the recorded classification along with the
    // placeholder ownership.
    assert!(ctx
        .provider
        .subscription_ownership
        .lock()
        .await
        .get(&pending)
        .is_none());

    // A later fetch at a slot at or below the rejected update's slot must
    // re-run the full classification: the found account goes through the
    // secondary tier and is rejected again, instead of losing arbitration
    // to the stale classification and being returned without admission.
    ctx.rpc_client.add_account(
        pending,
        Account {
            lamports: 1,
            ..Default::default()
        },
    );
    let err = ctx
        .provider
        .try_get(pending, AccountFetchContext::rpc_get_account())
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        RemoteAccountProviderError::AccountResolutionsFailed(message)
            if message.contains("NoEvictableSubscriptionCapacity")
                && message.contains(&pending.to_string())
    ));
    assert!(!ctx.provider.is_watching(&pending));
}

#[tokio::test]
async fn test_subscription_creation_rejection_survives_unsubscribe_failure() {
    init_logger();
    // Serializes with tests that read cleanup metric deltas; this test
    // emits the same metrics.
    let _metric_guard = SUBSCRIPTION_LIFECYCLE_METRIC_TEST_GUARD.lock().await;

    let protected = random_pubkey();
    let missing = random_pubkey();
    let ctx = setup_provider_with_lru_capacity(
        protected,
        Account {
            lamports: 1,
            ..Default::default()
        },
        1,
    )
    .await;
    ctx.pubsub_client.set_transport(PubsubTransport::Grpc);
    ctx.provider.abort_subscription_reconciler_for_test().await;
    let mut removed_rx = ctx.provider.try_get_removed_account_rx().unwrap();

    ctx.provider
        .try_get(protected, AccountFetchContext::rpc_get_account())
        .await
        .unwrap();
    ctx.provider
        .acquire_subscription(
            &protected,
            SubscriptionReason::UndelegationTracking,
        )
        .await
        .unwrap();
    let fetch_slot = ctx
        .provider
        .try_get(missing, AccountFetchContext::rpc_get_account())
        .await
        .unwrap()
        .slot();

    // The rejection decision is final even when the unsubscribe fails:
    // tier state and classification are dropped and the removal
    // notification still goes out; only the stray pubsub subscription is
    // left for the reconciler.
    ctx.pubsub_client.fail_next_unsubscriptions(1);
    ctx.pubsub_client
        .send_account_update(
            missing,
            fetch_slot + 1,
            &Account {
                lamports: 1,
                ..Default::default()
            },
        )
        .await;
    let provider = ctx.provider.clone();
    wait_until("rejected promotion to drop tier state", || {
        !provider.secondary_subscriptions.contains(&missing)
    })
    .await;
    assert!(!ctx.provider.is_watching(&missing));
    wait_until_ownership_removed(&ctx.provider, &missing).await;
    assert_eq!(wait_for_removed_account(&mut removed_rx).await, missing);
    assert!(ctx.pubsub_client.subscriptions_union().contains(&missing));

    // A reconciler pass removes the stray subscription.
    ctx.provider.reconcile_subscriptions_once_for_test().await;
    assert!(!ctx.pubsub_client.subscriptions_union().contains(&missing));
}

async fn direct_account_refcount(
    provider: &RemoteAccountProvider<ChainRpcClientMock, ChainPubsubClientMock>,
    pubkey: &Pubkey,
) -> usize {
    provider
        .subscription_ownership
        .lock()
        .await
        .get(pubkey)
        .and_then(|ownership| {
            ownership
                .reasons
                .get(&SubscriptionReason::DirectAccount)
                .copied()
        })
        .unwrap_or(0)
}

#[tokio::test]
async fn test_promotion_evicted_mid_flight_is_not_treated_as_admitted() {
    init_logger();

    let existing = random_pubkey();
    let missing = random_pubkey();
    let ctx = setup_provider(
        existing,
        Account {
            lamports: 1,
            ..Default::default()
        },
    )
    .await;
    ctx.pubsub_client.set_transport(PubsubTransport::Grpc);

    // A key promoted by another transition holds primary membership and
    // reports the benign departure outcome.
    assert!(ctx
        .provider
        .try_get(existing, AccountFetchContext::rpc_get_account())
        .await
        .unwrap()
        .is_found());
    assert!(ctx
        .provider
        .lrucache_subscribed_accounts
        .contains(&existing));
    assert_eq!(
        ctx.provider
            .subscription_tier_ctx()
            .try_promote_found_to_primary(existing, None)
            .await
            .unwrap(),
        PromotionOutcome::NotInSecondary
    );

    // Establish a confirmed-missing secondary entry.
    assert!(!ctx
        .provider
        .try_get(missing, AccountFetchContext::rpc_get_account())
        .await
        .unwrap()
        .is_found());
    assert!(ctx.provider.secondary_subscriptions.contains(&missing));

    // Evict the key from the secondary tier while the promotion awaits the
    // coverage-restoring subscribe; the promotion must not count the found
    // result as admitted.
    ctx.pubsub_client.pause_after_subscribe_insert();
    let insertions_before = ctx.pubsub_client.subscribe_insertions();
    let tier_ctx = ctx.provider.subscription_tier_ctx();
    let promotion = tokio::spawn(async move {
        tier_ctx.try_promote_found_to_primary(missing, None).await
    });
    ctx.pubsub_client
        .wait_for_subscribe_insertions(insertions_before + 1)
        .await;
    ctx.provider.secondary_subscriptions.remove(&missing);
    ctx.pubsub_client.resume_after_subscribe_insert();

    let outcome = tokio::time::timeout(Duration::from_secs(2), promotion)
        .await
        .expect("timed out waiting for promotion")
        .expect("promotion task should not panic")
        .expect("promotion should not error");
    assert_eq!(outcome, PromotionOutcome::Evicted);
    assert!(!ctx.provider.lrucache_subscribed_accounts.contains(&missing));
}

#[tokio::test]
async fn test_failed_coverage_restore_rolls_back_acquired_reason() {
    init_logger();

    let existing = random_pubkey();
    let missing = random_pubkey();
    let ctx = setup_provider(
        existing,
        Account {
            lamports: 1,
            ..Default::default()
        },
    )
    .await;
    ctx.provider.abort_subscription_reconciler_for_test().await;
    ctx.pubsub_client.set_transport(PubsubTransport::Grpc);

    assert!(!ctx
        .provider
        .try_get(missing, AccountFetchContext::rpc_get_account())
        .await
        .unwrap()
        .is_found());
    let refcount_before =
        direct_account_refcount(&ctx.provider, &missing).await;

    // Both the gRPC repair and full-coverage fallback fail transiently; the
    // just-acquired reason must be rolled back.
    ctx.pubsub_client.simulate_disconnect();
    assert!(ctx
        .provider
        .acquire_subscription_with_origin(
            &missing,
            SubscriptionReason::DirectAccount,
            SubscriptionRegistrationOrigin::Fetch(
                AccountFetchContext::rpc_get_account(),
            ),
        )
        .await
        .is_err());

    assert_eq!(
        direct_account_refcount(&ctx.provider, &missing).await,
        refcount_before
    );
}

#[tokio::test]
async fn test_fetch_owned_secondary_arms_grpc_only_and_promotes_found() {
    init_logger();

    let existing = random_pubkey();
    let missing = random_pubkey();
    let ctx = setup_provider(
        existing,
        Account {
            lamports: 1,
            ..Default::default()
        },
    )
    .await;
    ctx.pubsub_client.set_transport(PubsubTransport::Grpc);

    // Fetch-owned watches are armed gRPC-only from the start; a found result
    // restores full coverage on promotion to the primary tier.
    let subscribe_attempts = ctx.pubsub_client.subscribe_attempts();
    assert!(ctx
        .provider
        .try_get(existing, AccountFetchContext::rpc_get_account())
        .await
        .unwrap()
        .is_found());
    assert_eq!(ctx.pubsub_client.prefer_grpc_calls(), vec![existing]);
    // The gRPC-only arm plus the full-coverage restore after the found
    // classification each subscribe once.
    assert_eq!(
        ctx.pubsub_client.subscribe_attempts(),
        subscribe_attempts + 2
    );

    // A miss never leaves gRPC-only coverage: the confirming not-found
    // classification reaffirms the policy without transport work.
    assert!(!ctx
        .provider
        .try_get(missing, AccountFetchContext::rpc_get_account())
        .await
        .unwrap()
        .is_found());
    assert_eq!(
        ctx.pubsub_client.prefer_grpc_calls(),
        vec![existing, missing, missing]
    );
    assert!(ctx.provider.secondary_subscriptions.contains(&missing));
    // The mock is a gRPC client, so coverage stays after the switch.
    assert!(ctx.pubsub_client.subscriptions_union().contains(&missing));
}

#[tokio::test]
async fn test_cancelled_secondary_promotion_restores_previous_authority() {
    init_logger();

    let existing = random_pubkey();
    let missing = random_pubkey();
    let ctx = setup_multiplexed_provider(existing, Account::default()).await;
    ctx.provider.abort_subscription_reconciler_for_test().await;

    assert!(!ctx
        .provider
        .try_get(missing, AccountFetchContext::rpc_get_account())
        .await
        .unwrap()
        .is_found());
    assert!(ctx.provider.secondary_subscriptions.contains(&missing));
    assert!(ctx.grpc_client.is_subscribed(&missing));
    assert!(!ctx.websocket_client.is_subscribed(&missing));

    ctx.websocket_client.pause_after_subscribe_insert();
    ctx.grpc_client.pause_after_subscribe_insert();
    let websocket_insertions_before =
        ctx.websocket_client.subscribe_insertions();
    let grpc_insertions_before = ctx.grpc_client.subscribe_insertions();
    let provider = ctx.provider.clone();
    let promotion = tokio::spawn(async move {
        provider
            .acquire_subscription(
                &missing,
                SubscriptionReason::DelegationRecord,
            )
            .await
    });
    ctx.websocket_client
        .wait_for_subscribe_insertions(websocket_insertions_before + 1)
        .await;
    ctx.grpc_client
        .wait_for_subscribe_insertions(grpc_insertions_before + 1)
        .await;

    promotion.abort();
    assert!(
        promotion.await.unwrap_err().is_cancelled(),
        "promotion caller should be cancelled"
    );

    let subscription_key_locks = ctx.provider.subscription_key_locks.clone();
    let (waiter_started_tx, waiter_started_rx) = oneshot::channel();
    let (acquired_tx, mut acquired_rx) = oneshot::channel();
    let same_key = tokio::spawn(async move {
        let _ = waiter_started_tx.send(());
        let guard = subscription_key_owned_guard_from_map(
            &subscription_key_locks,
            missing,
        )
        .await;
        let _ = acquired_tx.send(guard);
    });
    waiter_started_rx.await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(25), &mut acquired_rx,)
            .await
            .is_err(),
        "promotion transaction must retain the key through SubMux fanout"
    );

    // Rollback waits for every original SubMux leg to settle before restoring
    // the previous gRPC-only secondary authority.
    ctx.grpc_client.resume_after_subscribe_insert();
    ctx.websocket_client.resume_after_subscribe_insert();
    let same_key_guard =
        tokio::time::timeout(Duration::from_secs(2), &mut acquired_rx)
            .await
            .expect("timed out waiting for promotion transaction")
            .expect("same-key waiter sender should remain live");
    drop(same_key_guard);
    same_key.await.unwrap();

    assert!(!ctx.provider.lrucache_subscribed_accounts.contains(&missing));
    assert!(ctx.provider.secondary_subscriptions.contains(&missing));
    assert!(ctx.grpc_client.is_subscribed(&missing));
    assert!(!ctx.websocket_client.is_subscribed(&missing));
    assert!(
        ctx.provider
            .has_subscription_reason(
                &missing,
                SubscriptionReason::DirectAccount
            )
            .await
    );
    assert!(
        !ctx.provider
            .has_subscription_reason(
                &missing,
                SubscriptionReason::DelegationRecord
            )
            .await
    );
}

#[tokio::test]
async fn test_read_rpc_secondary_avoids_websocket_transport_work() {
    init_logger();

    let existing = random_pubkey();
    let missing = random_pubkey();
    let ctx = setup_multiplexed_provider(existing, Account::default()).await;
    let ws_subscribe_baseline = ctx.websocket_client.subscribe_attempts();
    let ws_unsubscribe_baseline = ctx.websocket_client.unsubscribe_attempts();
    let grpc_subscribe_baseline = ctx.grpc_client.subscribe_attempts();

    ctx.rpc_client.block_fetches();
    let task_handle = tokio::spawn({
        let provider = ctx.provider.clone();
        async move {
            provider
                .try_get(missing, AccountFetchContext::rpc_get_account())
                .await
        }
    });
    wait_until("read RPC fetch to enter the secondary tier", || {
        ctx.provider.is_pending(&missing)
            && ctx.provider.secondary_subscriptions.contains(&missing)
    })
    .await;
    assert_eq!(
        ctx.websocket_client.subscribe_attempts(),
        ws_subscribe_baseline
    );
    assert_eq!(
        ctx.websocket_client.unsubscribe_attempts(),
        ws_unsubscribe_baseline
    );
    assert_eq!(
        ctx.grpc_client.subscribe_attempts(),
        grpc_subscribe_baseline + 1
    );

    // A reconciler pass must preserve the pending secondary policy without
    // introducing a websocket leg.
    let never_evicted = ctx
        .provider
        .lrucache_subscribed_accounts
        .never_evicted_accounts();
    subscription_reconciler::reconcile_subscriptions(
        &ctx.provider.lrucache_subscribed_accounts,
        &ctx.provider.secondary_subscriptions,
        &ctx.provider.pubsub_client,
        &never_evicted,
        &ctx.provider.removed_account_tx,
        Some(&ctx.provider.subscription_key_locks),
        Some(&ctx.provider.subscription_transition_lock),
        Some(&ctx.provider.subscription_ownership),
        Some(ctx.provider.fetching_accounts.as_ref()),
        Some(&ctx.provider.capacity_eviction_protection),
    )
    .await;
    assert_eq!(
        ctx.websocket_client.subscribe_attempts(),
        ws_subscribe_baseline
    );
    assert_eq!(
        ctx.websocket_client.unsubscribe_attempts(),
        ws_unsubscribe_baseline
    );

    ctx.rpc_client.allow_fetches();
    let remote_account =
        tokio::time::timeout(Duration::from_secs(2), task_handle)
            .await
            .expect("fetch should complete")
            .expect("fetch should not panic")
            .expect("fetch should succeed");
    assert!(!remote_account.is_found());
    assert_eq!(
        ctx.grpc_client.subscribe_attempts(),
        grpc_subscribe_baseline + 1,
        "not-found classification should be transport-idempotent"
    );
    assert_eq!(
        ctx.websocket_client.unsubscribe_attempts(),
        ws_unsubscribe_baseline
    );

    // Refetching the confirmed miss remains gRPC-only.
    assert!(!ctx
        .provider
        .try_get(missing, AccountFetchContext::rpc_get_account())
        .await
        .unwrap()
        .is_found());
    assert_eq!(
        ctx.websocket_client.subscribe_attempts(),
        ws_subscribe_baseline
    );
    assert_eq!(
        ctx.websocket_client.unsubscribe_attempts(),
        ws_unsubscribe_baseline
    );
    assert_eq!(
        ctx.grpc_client.subscribe_attempts(),
        grpc_subscribe_baseline + 1
    );

    // A found account restores full coverage before entering the primary
    // working set.
    assert!(ctx
        .provider
        .try_get(existing, AccountFetchContext::rpc_get_account())
        .await
        .unwrap()
        .is_found());
    assert_eq!(
        ctx.websocket_client.subscribe_attempts(),
        ws_subscribe_baseline + 1
    );
    assert_eq!(
        ctx.websocket_client.unsubscribe_attempts(),
        ws_unsubscribe_baseline
    );
    assert!(ctx
        .provider
        .lrucache_subscribed_accounts
        .contains(&existing));
    assert!(!ctx.provider.secondary_subscriptions.contains(&existing));
}

#[tokio::test]
async fn test_transaction_waiter_escalates_read_rpc_pending_fetch() {
    init_logger();

    let existing = random_pubkey();
    let missing = random_pubkey();
    let ctx = setup_multiplexed_provider(existing, Account::default()).await;
    ctx.rpc_client.block_fetches();

    let read_owner = tokio::spawn({
        let provider = ctx.provider.clone();
        async move {
            provider
                .try_get(missing, AccountFetchContext::rpc_get_account())
                .await
        }
    });
    wait_until("read owner to establish gRPC-first coverage", || {
        ctx.provider.is_pending(&missing)
            && ctx.grpc_client.is_subscribed(&missing)
            && !ctx.websocket_client.is_subscribed(&missing)
    })
    .await;

    let transaction_waiter = tokio::spawn({
        let provider = ctx.provider.clone();
        async move {
            provider
                .try_get(
                    missing,
                    AccountFetchContext::send_transaction(
                        solana_signature::Signature::new_unique(),
                    ),
                )
                .await
        }
    });
    wait_until("transaction waiter to restore full coverage", || {
        ctx.provider
            .fetching_accounts
            .lock()
            .unwrap()
            .get(&missing)
            .is_some_and(|state| state.requires_full_coverage.requires_full())
            && ctx.websocket_client.is_subscribed(&missing)
    })
    .await;

    ctx.provider.reconcile_subscriptions_once_for_test().await;
    assert!(
        ctx.websocket_client.is_subscribed(&missing),
        "reconciliation must preserve the waiter's full-coverage requirement"
    );

    ctx.rpc_client.allow_fetches();
    let read_result = tokio::time::timeout(Duration::from_secs(2), read_owner)
        .await
        .expect("read owner should complete")
        .expect("read owner should not panic")
        .expect("read owner should succeed");
    let transaction_result =
        tokio::time::timeout(Duration::from_secs(2), transaction_waiter)
            .await
            .expect("transaction waiter should complete")
            .expect("transaction waiter should not panic")
            .expect("transaction waiter should succeed");
    assert!(!read_result.is_found());
    assert!(!transaction_result.is_found());
    wait_until("not-found result to restore gRPC preference", || {
        !ctx.websocket_client.is_subscribed(&missing)
    })
    .await;
}

#[tokio::test]
async fn test_transaction_batch_escalates_joined_fetches_before_new_setup() {
    init_logger();

    let existing = random_pubkey();
    let joined_a = random_pubkey();
    let joined_b = random_pubkey();
    let newly_claimed = random_pubkey();
    let ctx = setup_multiplexed_provider(existing, Account::default()).await;
    ctx.rpc_client.block_fetches();

    let read_owner_a = tokio::spawn({
        let provider = ctx.provider.clone();
        async move {
            provider
                .try_get(joined_a, AccountFetchContext::rpc_get_account())
                .await
        }
    });
    let read_owner_b = tokio::spawn({
        let provider = ctx.provider.clone();
        async move {
            provider
                .try_get(joined_b, AccountFetchContext::rpc_get_account())
                .await
        }
    });
    wait_until("read owners to establish gRPC-first coverage", || {
        [joined_a, joined_b].into_iter().all(|pubkey| {
            ctx.provider.is_pending(&pubkey)
                && ctx.grpc_client.is_subscribed(&pubkey)
                && !ctx.websocket_client.is_subscribed(&pubkey)
        })
    })
    .await;

    ctx.websocket_client.pause_after_subscribe_insert();
    ctx.grpc_client.pause_after_subscribe_insert();
    let ws_insertions = ctx.websocket_client.subscribe_insertions();
    let grpc_insertions = ctx.grpc_client.subscribe_insertions();

    let transaction_batch = tokio::spawn({
        let provider = ctx.provider.clone();
        async move {
            provider
                .try_get_multi(
                    &[joined_a, joined_b, newly_claimed],
                    None,
                    AccountFetchContext::send_transaction(
                        solana_signature::Signature::new_unique(),
                    ),
                    None,
                )
                .await
        }
    });

    tokio::time::timeout(
        Duration::from_secs(2),
        ctx.websocket_client
            .wait_for_subscribe_insertions(ws_insertions + 2),
    )
    .await
    .expect("both joined keys should begin websocket escalation concurrently");
    tokio::time::timeout(
        Duration::from_secs(2),
        ctx.grpc_client
            .wait_for_subscribe_insertions(grpc_insertions + 2),
    )
    .await
    .expect("both joined keys should begin gRPC escalation concurrently");
    for joined in [joined_a, joined_b] {
        assert!(ctx.websocket_client.is_subscribed(&joined));
        assert!(ctx.grpc_client.is_subscribed(&joined));
    }
    assert!(!ctx.websocket_client.is_subscribed(&newly_claimed));
    assert!(!ctx.grpc_client.is_subscribed(&newly_claimed));

    ctx.websocket_client.resume_after_subscribe_insert();
    ctx.grpc_client.resume_after_subscribe_insert();
    wait_until("new key setup to follow joined-key escalation", || {
        ctx.websocket_client.is_subscribed(&newly_claimed)
            && ctx.grpc_client.is_subscribed(&newly_claimed)
    })
    .await;
    ctx.rpc_client.allow_fetches();

    for read_owner in [read_owner_a, read_owner_b] {
        assert!(!tokio::time::timeout(Duration::from_secs(2), read_owner)
            .await
            .expect("read owner should complete")
            .expect("read owner should not panic")
            .expect("read owner should succeed")
            .is_found());
    }
    let transaction_results =
        tokio::time::timeout(Duration::from_secs(2), transaction_batch)
            .await
            .expect("transaction batch should complete")
            .expect("transaction batch should not panic")
            .expect("transaction batch should succeed");
    assert_eq!(transaction_results.len(), 3);
    assert!(transaction_results
        .into_iter()
        .all(|account| !account.is_found()));
}

#[tokio::test]
async fn test_terminal_resolution_honors_full_requirement_after_key_lock() {
    init_logger();

    let existing = random_pubkey();
    let missing = random_pubkey();
    let ctx = setup_multiplexed_provider(existing, Account::default()).await;
    let missing_result = ctx
        .provider
        .try_get(missing, AccountFetchContext::rpc_get_account())
        .await
        .expect("initial read should succeed");
    assert!(!missing_result.is_found());
    assert!(ctx.grpc_client.is_subscribed(&missing));
    assert!(!ctx.websocket_client.is_subscribed(&missing));

    let generation = ctx.provider.next_fetching_account_generation();
    let coverage = Arc::new(PendingFetchCoverageRequirement::new(false));
    ctx.provider.fetching_accounts.lock().unwrap().insert(
        missing,
        FetchingAccountState {
            generation,
            fetch_start_slot: 100,
            fetch_context: AccountFetchContext::rpc_get_account(),
            requires_full_coverage: Arc::clone(&coverage),
            owner_started_at: std::time::Instant::now(),
            waiters: Vec::new(),
        },
    );

    // Model the terminal RPC resolver winning the per-key lock before a
    // transaction joins the still-open pending generation.
    let subscription_guard =
        ctx.provider.subscription_key_guard(&missing).await;
    let transaction_waiter = tokio::spawn({
        let provider = ctx.provider.clone();
        async move {
            provider
                .try_get(
                    missing,
                    AccountFetchContext::send_transaction(
                        solana_signature::Signature::new_unique(),
                    ),
                )
                .await
        }
    });
    wait_until("transaction waiter to publish its full requirement", || {
        coverage.requires_full()
            && ctx
                .provider
                .fetching_accounts
                .lock()
                .unwrap()
                .get(&missing)
                .is_some_and(|state| state.waiters.len() == 1)
    })
    .await;

    let state = remove_fetching_account_if_generation_matches(
        &mut ctx.provider.fetching_accounts.lock().unwrap(),
        &missing,
        generation,
    )
    .expect("terminal resolver should still own the generation");
    let retain_full_until_resolution = state.requires_full_coverage.resolve();
    assert!(retain_full_until_resolution);
    assert!(
        !coverage.require_full(),
        "coverage admission must close atomically at terminal resolution"
    );
    let classification = ctx
        .provider
        .subscription_tier_ctx()
        .apply_fetch_classification(
            &missing,
            101,
            true,
            retain_full_until_resolution,
            subscription_guard,
        )
        .await
        .expect("terminal classification should succeed");
    assert!(classification.prefer_grpc_after_resolution);
    assert!(
        ctx.websocket_client.is_subscribed(&missing),
        "full coverage must be established before terminal waiters are notified"
    );

    // A newer Full generation may publish under the fetching lock while the
    // old terminal outcome still owns the per-key guard. It must not resolve
    // and disappear before the old outcome performs its final recheck.
    let newer_generation = ctx.provider.next_fetching_account_generation();
    let newer_coverage = Arc::new(PendingFetchCoverageRequirement::new(true));
    ctx.provider.fetching_accounts.lock().unwrap().insert(
        missing,
        FetchingAccountState {
            generation: newer_generation,
            fetch_start_slot: 102,
            fetch_context: AccountFetchContext::rpc_get_account(),
            requires_full_coverage: Arc::clone(&newer_coverage),
            owner_started_at: std::time::Instant::now(),
            waiters: Vec::new(),
        },
    );
    let (newer_started_tx, newer_started_rx) = oneshot::channel();
    let newer_terminal = tokio::spawn({
        let provider = ctx.provider.clone();
        async move {
            let _ = newer_started_tx.send(());
            let _subscription_guard =
                provider.subscription_key_guard(&missing).await;
            let state = remove_fetching_account_if_generation_matches(
                &mut provider.fetching_accounts.lock().unwrap(),
                &missing,
                newer_generation,
            )
            .expect("newer terminal should own its generation");
            state.requires_full_coverage.resolve()
        }
    });
    newer_started_rx
        .await
        .expect("newer terminal task should start");
    tokio::task::yield_now().await;
    assert!(
        !newer_terminal.is_finished(),
        "old terminal outcome must retain the key guard through notification"
    );

    for waiter in state.waiters {
        let _ = waiter.send(Ok(missing_result.clone()));
    }
    ctx.provider
        .subscription_tier_ctx()
        .prefer_grpc_after_fetch_resolution(
            missing,
            classification.subscription_guard,
        )
        .await;
    assert!(
        ctx.websocket_client.is_subscribed(&missing),
        "old terminal downgrade must preserve a newer Full generation"
    );
    assert!(
        newer_terminal
            .await
            .expect("newer terminal should not panic"),
        "newer Full generation should retain full coverage"
    );

    let transaction_result =
        tokio::time::timeout(Duration::from_secs(2), transaction_waiter)
            .await
            .expect("transaction waiter should complete")
            .expect("transaction waiter should not panic")
            .expect("transaction waiter should succeed");
    assert!(!transaction_result.is_found());

    let final_guard = ctx.provider.subscription_key_guard(&missing).await;
    ctx.provider
        .subscription_tier_ctx()
        .prefer_grpc_after_fetch_resolution(missing, final_guard)
        .await;
    assert!(!ctx.websocket_client.is_subscribed(&missing));
}

#[tokio::test]
async fn test_cancelled_grpc_first_admission_restores_absence() {
    init_logger();

    let existing = random_pubkey();
    let missing = random_pubkey();
    let ctx = setup_multiplexed_provider(existing, Account::default()).await;
    ctx.grpc_client.pause_after_subscribe_insert();
    let insertions_before = ctx.grpc_client.subscribe_insertions();
    let completions_before =
        ctx.provider.pubsub_client.grpc_preference_completions();

    let provider = ctx.provider.clone();
    let setup = tokio::spawn(async move {
        provider
            .try_get(missing, AccountFetchContext::rpc_get_account())
            .await
    });
    ctx.grpc_client
        .wait_for_subscribe_insertions(insertions_before + 1)
        .await;

    // Hold publication after gRPC preference succeeds, then cancel the caller.
    // Recovery retains the key guard while returning to the original absence.
    let transition_guard =
        ctx.provider.subscription_transition_lock.lock().await;
    ctx.grpc_client.resume_after_subscribe_insert();
    wait_until("gRPC preference to complete", || {
        ctx.provider.pubsub_client.grpc_preference_completions()
            > completions_before
    })
    .await;
    setup.abort();
    assert!(
        setup.await.unwrap_err().is_cancelled(),
        "subscription setup should be cancelled at LRU publication"
    );
    assert!(!ctx.provider.is_pending(&missing));
    assert!(!ctx.provider.lrucache_subscribed_accounts.contains(&missing));
    assert!(!ctx.provider.secondary_subscriptions.contains(&missing));

    let subscription_key_locks = ctx.provider.subscription_key_locks.clone();
    let (waiter_started_tx, waiter_started_rx) = oneshot::channel();
    let (acquired_tx, mut acquired_rx) = oneshot::channel();
    let same_key = tokio::spawn(async move {
        let _ = waiter_started_tx.send(());
        let guard = subscription_key_owned_guard_from_map(
            &subscription_key_locks,
            missing,
        )
        .await;
        let _ = acquired_tx.send(guard);
    });
    waiter_started_rx.await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(25), &mut acquired_rx,)
            .await
            .is_err(),
        "cancelled caller must not release the admission's key guard"
    );

    drop(transition_guard);
    let same_key_guard =
        tokio::time::timeout(Duration::from_secs(2), &mut acquired_rx)
            .await
            .expect("timed out waiting for admission transaction")
            .expect("same-key waiter sender should remain live");
    drop(same_key_guard);
    same_key.await.unwrap();
    assert!(!ctx.provider.secondary_subscriptions.contains(&missing));
    assert!(!ctx.provider.lrucache_subscribed_accounts.contains(&missing));
    assert!(!ctx.grpc_client.is_subscribed(&missing));
    assert!(!ctx.websocket_client.is_subscribed(&missing));
    assert!(!ctx
        .provider
        .subscription_ownership
        .lock()
        .await
        .contains_key(&missing));

    // A subsequently attached gRPC client must not resurrect the cancelled
    // provisional admission.
    let (updates_sender, updates_receiver) = mpsc::channel(1_000);
    let attached_grpc =
        Arc::new(ChainPubsubClientMock::new(updates_sender, updates_receiver));
    attached_grpc.set_transport(PubsubTransport::Grpc);
    let (_abort_sender, abort_receiver) = mpsc::channel(1);
    let tracker = Arc::new(TieredSubscribedAccountsTracker::new(
        ctx.provider.lrucache_subscribed_accounts.clone(),
        ctx.provider.secondary_subscriptions.clone(),
    ));
    ctx.provider
        .pubsub_client
        .add_client(attached_grpc.clone(), abort_receiver, tracker)
        .await
        .unwrap();
    assert!(attached_grpc.is_subscribed(&clock::ID));
    assert!(!attached_grpc.is_subscribed(&missing));
}

#[tokio::test]
async fn test_failed_secondary_repair_revokes_reconnect_authority_before_eviction(
) {
    init_logger();

    let existing = random_pubkey();
    let missing = random_pubkey();
    let ctx = setup_multiplexed_provider(existing, Account::default()).await;
    ctx.provider.abort_subscription_reconciler_for_test().await;
    assert!(!ctx
        .provider
        .try_get(missing, AccountFetchContext::rpc_get_account())
        .await
        .unwrap()
        .is_found());
    assert!(ctx.provider.secondary_subscriptions.contains(&missing));

    // Leave both clients visible to SubMux but unable to repair coverage.
    // Preference and full-coverage fallback both fail, making the reconciler
    // finalize eviction of the dead secondary watch.
    ctx.websocket_client.simulate_disconnect();
    ctx.grpc_client.simulate_disconnect();
    let never_evicted = ctx
        .provider
        .lrucache_subscribed_accounts
        .never_evicted_accounts();
    subscription_reconciler::reconcile_subscriptions(
        &ctx.provider.lrucache_subscribed_accounts,
        &ctx.provider.secondary_subscriptions,
        &ctx.provider.pubsub_client,
        &never_evicted,
        &ctx.provider.removed_account_tx,
        Some(&ctx.provider.subscription_key_locks),
        Some(&ctx.provider.subscription_transition_lock),
        Some(&ctx.provider.subscription_ownership),
        Some(ctx.provider.fetching_accounts.as_ref()),
        Some(&ctx.provider.capacity_eviction_protection),
    )
    .await;
    assert!(!ctx.provider.secondary_subscriptions.contains(&missing));
    assert!(!ctx
        .provider
        .subscription_ownership
        .lock()
        .await
        .contains_key(&missing));

    // A later gRPC attachment must not resurrect policy for the key evicted
    // from the authoritative tracker.
    let (updates_sender, updates_receiver) = mpsc::channel(1_000);
    let attached_grpc =
        Arc::new(ChainPubsubClientMock::new(updates_sender, updates_receiver));
    attached_grpc.set_transport(PubsubTransport::Grpc);
    let (_abort_sender, abort_receiver) = mpsc::channel(1);
    let tracker = Arc::new(TieredSubscribedAccountsTracker::new(
        ctx.provider.lrucache_subscribed_accounts.clone(),
        ctx.provider.secondary_subscriptions.clone(),
    ));
    ctx.provider
        .pubsub_client
        .add_client(attached_grpc.clone(), abort_receiver, tracker)
        .await
        .unwrap();
    assert!(attached_grpc.is_subscribed(&clock::ID));
    assert!(!attached_grpc.is_subscribed(&missing));
}

#[tokio::test]
async fn test_setup_cancellation_preserves_pump_admitted_primary_membership() {
    init_logger();

    let existing = random_pubkey();
    let pending = random_pubkey();
    let ctx = setup_provider(existing, Account::default()).await;
    ctx.pubsub_client.set_transport(PubsubTransport::Grpc);

    let waiter_rx = insert_pending_fetch(&ctx.provider, pending, 100);
    let generation = {
        let fetching = ctx.provider.fetching_accounts.lock().unwrap();
        fetching.get(&pending).unwrap().generation
    };
    ctx.pubsub_client.insert_subscription(pending);
    ctx.pubsub_client
        .send_account_update(
            pending,
            101,
            &Account {
                lamports: 1,
                ..Default::default()
            },
        )
        .await;
    tokio::time::timeout(Duration::from_secs(2), waiter_rx)
        .await
        .expect("timed out waiting for fetch resolution")
        .expect("waiter channel closed")
        .expect("subscription-resolved fetch should succeed");
    assert!(ctx.provider.lrucache_subscribed_accounts.contains(&pending));

    // The claiming try_get_multi future is cancelled before setup adopted
    // the placeholder; the cleanup must keep the ownership entry because
    // the update pump already admitted the key into the primary tier.
    cleanup_classification_placeholders(
        &ctx.provider.subscription_ownership,
        &ctx.provider.subscription_transition_lock,
        &ctx.provider.lrucache_subscribed_accounts,
        &ctx.provider.secondary_subscriptions,
        &[(pending, generation)].into_iter().collect(),
    )
    .await;
    assert!(ctx
        .provider
        .subscription_ownership
        .lock()
        .await
        .contains_key(&pending));
    assert!(ctx.provider.lrucache_subscribed_accounts.contains(&pending));

    // A later fetch adopts the membership instead of registering the key
    // as fetch-owned secondary alongside its primary entry.
    ctx.rpc_client.add_account(
        pending,
        Account {
            lamports: 1,
            ..Default::default()
        },
    );
    ctx.provider
        .try_get(pending, AccountFetchContext::rpc_get_account())
        .await
        .unwrap();
    assert!(ctx.provider.lrucache_subscribed_accounts.contains(&pending));
    assert!(!ctx.provider.secondary_subscriptions.contains(&pending));
    assert!(
        ctx.provider
            .has_subscription_reason(
                &pending,
                SubscriptionReason::DirectAccount
            )
            .await
    );
}

#[tokio::test]
async fn test_repeated_not_found_fetch_preserves_primary_working_set() {
    init_logger();

    let existing = random_pubkey();
    let missing = random_pubkey();
    let ctx = setup_provider_with_lru_capacity(
        existing,
        Account {
            lamports: 500,
            ..Default::default()
        },
        1,
    )
    .await;
    ctx.pubsub_client.set_transport(PubsubTransport::Grpc);

    assert!(ctx
        .provider
        .try_get(existing, AccountFetchContext::rpc_get_account())
        .await
        .unwrap()
        .is_found());
    assert!(ctx
        .provider
        .lrucache_subscribed_accounts
        .contains(&existing));

    for _ in 0..2 {
        assert!(!ctx
            .provider
            .try_get(missing, AccountFetchContext::rpc_get_account())
            .await
            .unwrap()
            .is_found());
    }

    assert!(ctx
        .provider
        .lrucache_subscribed_accounts
        .contains(&existing));
    assert!(!ctx.provider.lrucache_subscribed_accounts.contains(&missing));
    assert!(ctx.provider.secondary_subscriptions.contains(&missing));
    assert!(!ctx.provider.secondary_subscriptions.contains(&existing));
}

#[tokio::test]
async fn test_transaction_refetch_of_secondary_miss_restores_full_coverage() {
    init_logger();

    let existing = random_pubkey();
    let missing = random_pubkey();
    let ctx = setup_multiplexed_provider(existing, Account::default()).await;

    let fetch_context = AccountFetchContext::send_transaction(
        solana_signature::Signature::new_unique(),
    );
    assert!(!ctx
        .provider
        .try_get(missing, fetch_context)
        .await
        .unwrap()
        .is_found());
    assert!(ctx.provider.secondary_subscriptions.contains(&missing));
    assert!(ctx.grpc_client.is_subscribed(&missing));
    assert!(!ctx.websocket_client.is_subscribed(&missing));
    let ws_subscribe_attempts = ctx.websocket_client.subscribe_attempts();

    ctx.rpc_client.block_fetches();
    let task_handle = tokio::spawn({
        let provider = ctx.provider.clone();
        async move { provider.try_get(missing, fetch_context).await }
    });
    let provider = ctx.provider.clone();
    wait_until("transaction refetch to restore full coverage", || {
        provider.is_pending(&missing)
            && provider.secondary_subscriptions.contains(&missing)
            && ctx.websocket_client.is_subscribed(&missing)
    })
    .await;
    assert_eq!(
        ctx.websocket_client.subscribe_attempts(),
        ws_subscribe_attempts + 1
    );
    assert!(ctx.grpc_client.is_subscribed(&missing));

    let never_evicted = ctx
        .provider
        .lrucache_subscribed_accounts
        .never_evicted_accounts();
    subscription_reconciler::reconcile_subscriptions(
        &ctx.provider.lrucache_subscribed_accounts,
        &ctx.provider.secondary_subscriptions,
        &ctx.provider.pubsub_client,
        &never_evicted,
        &ctx.provider.removed_account_tx,
        Some(&ctx.provider.subscription_key_locks),
        Some(&ctx.provider.subscription_transition_lock),
        Some(&ctx.provider.subscription_ownership),
        Some(ctx.provider.fetching_accounts.as_ref()),
        Some(&ctx.provider.capacity_eviction_protection),
    )
    .await;
    assert!(
        ctx.websocket_client.is_subscribed(&missing),
        "reconciliation must preserve full coverage for a pending transaction fetch"
    );

    ctx.rpc_client.allow_fetches();
    let remote_account =
        tokio::time::timeout(Duration::from_secs(2), task_handle)
            .await
            .expect("refetch should complete")
            .expect("refetch should not panic")
            .expect("refetch should succeed");
    assert!(!remote_account.is_found());
    wait_until("not-found result to restore gRPC preference", || {
        !ctx.websocket_client.is_subscribed(&missing)
    })
    .await;
}

#[tokio::test]
async fn test_manual_unsubscribe_removes_secondary_account() {
    init_logger();

    let existing = random_pubkey();
    let missing = random_pubkey();
    let ctx = setup_provider(existing, Account::default()).await;
    ctx.pubsub_client.set_transport(PubsubTransport::Grpc);
    let mut removed_rx = ctx.provider.try_get_removed_account_rx().unwrap();

    assert!(!ctx
        .provider
        .try_get(missing, AccountFetchContext::rpc_get_account())
        .await
        .unwrap()
        .is_found());
    assert!(ctx.provider.secondary_subscriptions.contains(&missing));

    ctx.provider.unsubscribe(&missing).await.unwrap();

    assert!(!ctx.provider.is_watching(&missing));
    assert!(!ctx.pubsub_client.subscriptions_union().contains(&missing));
    assert!(
        !ctx.provider
            .has_subscription_reason(
                &missing,
                SubscriptionReason::DirectAccount
            )
            .await
    );
    assert_eq!(removed_rx.recv().await, Some(missing));
}

#[tokio::test]
async fn test_failed_membership_repair_rolls_back_new_reason() {
    init_logger();

    let pubkey = random_pubkey();
    let ctx = setup_provider(pubkey, Account::default()).await;
    ctx.provider
        .acquire_subscription(&pubkey, SubscriptionReason::DirectAccount)
        .await
        .unwrap();

    ctx.provider.lrucache_subscribed_accounts.remove(&pubkey);
    ctx.pubsub_client.simulate_disconnect();
    assert!(ctx
        .provider
        .acquire_subscription(&pubkey, SubscriptionReason::DelegationRecord)
        .await
        .is_err());

    assert!(
        ctx.provider
            .has_subscription_reason(&pubkey, SubscriptionReason::DirectAccount)
            .await
    );
    assert!(
        !ctx.provider
            .has_subscription_reason(
                &pubkey,
                SubscriptionReason::DelegationRecord,
            )
            .await
    );
}

#[tokio::test]
async fn test_secondary_critical_acquire_fails_without_primary_capacity() {
    init_logger();

    let protected = random_pubkey();
    let missing = random_pubkey();
    let ctx =
        setup_provider_with_lru_capacity(protected, Account::default(), 1)
            .await;
    ctx.pubsub_client.set_transport(PubsubTransport::Grpc);

    ctx.provider
        .acquire_subscription(
            &protected,
            SubscriptionReason::UndelegationTracking,
        )
        .await
        .unwrap();
    assert!(!ctx
        .provider
        .try_get(missing, AccountFetchContext::rpc_get_account())
        .await
        .unwrap()
        .is_found());

    let err = ctx
        .provider
        .acquire_subscription(&missing, SubscriptionReason::DelegationRecord)
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        RemoteAccountProviderError::NoEvictableSubscriptionCapacity { pubkey }
            if pubkey == missing
    ));
    assert!(ctx
        .provider
        .lrucache_subscribed_accounts
        .contains(&protected));
    assert!(ctx.provider.secondary_subscriptions.contains(&missing));
    assert!(
        !ctx.provider
            .has_subscription_reason(
                &missing,
                SubscriptionReason::DelegationRecord,
            )
            .await
    );
}

#[tokio::test]
async fn test_secondary_capacity_preserves_protected_account() {
    init_logger();
    // Serializes with tests that read capacity-eviction cleanup metric
    // deltas; this test emits the same metrics.
    let _metric_guard = SUBSCRIPTION_LIFECYCLE_METRIC_TEST_GUARD.lock().await;

    let primary = random_pubkey();
    let protected_missing = random_pubkey();
    let rejected_missing = random_pubkey();
    let ctx = setup_provider_with_lru_capacity(
        primary,
        Account {
            lamports: 1,
            ..Default::default()
        },
        1,
    )
    .await;
    ctx.pubsub_client.set_transport(PubsubTransport::Grpc);
    ctx.provider.abort_subscription_reconciler_for_test().await;

    ctx.provider
        .acquire_subscription(
            &primary,
            SubscriptionReason::UndelegationTracking,
        )
        .await
        .unwrap();
    assert!(!ctx
        .provider
        .try_get(protected_missing, AccountFetchContext::rpc_get_account(),)
        .await
        .unwrap()
        .is_found());
    ctx.provider
        .acquire_subscription(
            &protected_missing,
            SubscriptionReason::UndelegationTracking,
        )
        .await
        .unwrap();

    let subscribe_attempts = ctx.pubsub_client.subscribe_attempts();
    ctx.pubsub_client.fail_next_unsubscriptions(1);
    let err = ctx
        .provider
        .try_get(rejected_missing, AccountFetchContext::rpc_get_account())
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        RemoteAccountProviderError::NoEvictableSubscriptionCapacity { .. }
    ));
    assert!(ctx
        .provider
        .secondary_subscriptions
        .contains(&protected_missing));
    assert!(ctx.provider.is_watching(&protected_missing));
    assert!(!ctx.provider.is_watching(&rejected_missing));
    assert!(!ctx
        .pubsub_client
        .subscriptions_union()
        .contains(&rejected_missing));
    assert_eq!(ctx.pubsub_client.subscribe_attempts(), subscribe_attempts);
}

#[tokio::test]
async fn test_secondary_eviction_unsubscribe_failure_keeps_admission() {
    init_logger();
    // Serializes with tests that read capacity-eviction cleanup metric
    // deltas; this test emits the same metrics.
    let _metric_guard = SUBSCRIPTION_LIFECYCLE_METRIC_TEST_GUARD.lock().await;

    let existing = random_pubkey();
    let first_missing = random_pubkey();
    let second_missing = random_pubkey();
    let ctx = setup_provider_with_lru_capacity(
        existing,
        Account {
            lamports: 1,
            ..Default::default()
        },
        1,
    )
    .await;
    ctx.pubsub_client.set_transport(PubsubTransport::Grpc);
    ctx.provider.abort_subscription_reconciler_for_test().await;

    assert!(!ctx
        .provider
        .try_get(first_missing, AccountFetchContext::rpc_get_account())
        .await
        .unwrap()
        .is_found());
    ctx.pubsub_client.fail_next_unsubscriptions(1);

    // The admission stands even though the evicted key's unsubscribe fails;
    // the stray subscription is removed by the reconciler on a later pass.
    assert!(!ctx
        .provider
        .try_get(second_missing, AccountFetchContext::rpc_get_account())
        .await
        .unwrap()
        .is_found());
    assert!(ctx
        .provider
        .secondary_subscriptions
        .contains(&second_missing));
    assert!(ctx.provider.is_watching(&second_missing));
    assert!(!ctx.provider.is_watching(&first_missing));
    wait_until_ownership_removed(&ctx.provider, &first_missing).await;
    // The failed unsubscribe leaves a stray pubsub subscription behind for
    // the reconciler to collect.
    assert!(ctx
        .pubsub_client
        .subscriptions_union()
        .contains(&first_missing));

    // A reconciler pass removes the stray subscription.
    ctx.provider.reconcile_subscriptions_once_for_test().await;
    assert!(!ctx
        .pubsub_client
        .subscriptions_union()
        .contains(&first_missing));
}

#[tokio::test]
async fn test_found_fetch_fails_when_primary_capacity_is_protected() {
    init_logger();

    let protected = random_pubkey();
    let found = random_pubkey();
    let ctx = setup_provider_with_lru_capacity(
        protected,
        Account {
            lamports: 1,
            ..Default::default()
        },
        1,
    )
    .await;
    ctx.rpc_client.add_account(
        found,
        Account {
            lamports: 1,
            ..Default::default()
        },
    );
    let mut removed_rx = ctx.provider.try_get_removed_account_rx().unwrap();
    ctx.provider
        .acquire_subscription(
            &protected,
            SubscriptionReason::UndelegationTracking,
        )
        .await
        .unwrap();

    let err = ctx
        .provider
        .try_get(found, AccountFetchContext::rpc_get_account())
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        RemoteAccountProviderError::AccountResolutionsFailed(message)
            if message.contains("NoEvictableSubscriptionCapacity")
                && message.contains(&found.to_string())
    ));
    assert!(ctx
        .provider
        .lrucache_subscribed_accounts
        .contains(&protected));
    assert!(!ctx.provider.secondary_subscriptions.contains(&found));
    assert!(!ctx.provider.is_watching(&found));
    assert!(!ctx.pubsub_client.subscriptions_union().contains(&found));
    // The rejected promotion dropped the last watch; the removal pipeline
    // must be notified so a stale bank entry cannot outlive it.
    assert_eq!(wait_for_removed_account(&mut removed_rx).await, found);
}

struct TestSlotConfig {
    current_slot: u64,
    account1_slot: u64,
    account2_slot: u64,
}

#[tokio::test]
async fn test_try_get_multi_short_multi_account_response_returns_error() {
    let _metrics_guard =
        crate::testing::pending_metric_test_lock().lock().await;
    init_logger();

    let pubkey1 = solana_pubkey::Pubkey::new_unique();
    let pubkey2 = solana_pubkey::Pubkey::new_unique();
    let account1 = Account {
        lamports: 1_000_000,
        data: vec![1, 2, 3, 4],
        owner: solana_pubkey::Pubkey::new_unique(),
        executable: false,
        rent_epoch: 0,
    };
    let account2 = Account {
        lamports: 2_000_000,
        data: vec![5, 6, 7, 8],
        owner: solana_pubkey::Pubkey::new_unique(),
        executable: false,
        rent_epoch: 0,
    };

    let rpc_client = ChainRpcClientMockBuilder::new()
        .slot(100)
        .clock_sysvar_for_slot(100)
        .account(pubkey1, account1)
        .account(pubkey2, account2)
        .truncate_multi_account_response_to(1)
        .build();

    let (updates_sender, updates_receiver) = mpsc::channel(1_000);
    let pubsub_client =
        ChainPubsubClientMock::new(updates_sender, updates_receiver);

    let (forward_tx, _forward_rx) = mpsc::channel(1_000);
    let (subscribed_accounts, config) = create_test_lru_cache(1000);
    let chain_slot = Arc::<AtomicU64>::default();

    let provider = RemoteAccountProvider::new(
        rpc_client,
        pubsub_client,
        forward_tx,
        &config,
        subscribed_accounts,
        ChainSlot::new(chain_slot),
    )
    .await
    .unwrap();

    let result = tokio::time::timeout(
        Duration::from_millis(500),
        provider.try_get_multi(
            &[pubkey1, pubkey2],
            None,
            AccountFetchContext::rpc_get_account(),
            None,
        ),
    )
    .await;

    let fetch_result = result.expect("try_get_multi should not hang");
    assert!(fetch_result.is_err());
}

async fn setup_matching_slots(
    config: TestSlotConfig,
    pubkey1: Pubkey,
    pubkey2: Pubkey,
) -> (
    RemoteAccountProvider<ChainRpcClientMock, ChainPubsubClientMock>,
    mpsc::Receiver<ForwardedSubscriptionUpdate>,
) {
    init_logger();

    let rpc_client = ChainRpcClientMockBuilder::new()
        .slot(config.current_slot)
        .account(
            pubkey1,
            Account {
                lamports: 555,
                data: vec![],
                owner: system_program::id(),
                executable: false,
                rent_epoch: 0,
            },
        )
        .account(
            pubkey2,
            Account {
                lamports: 666,
                data: vec![],
                owner: system_program::id(),
                executable: false,
                rent_epoch: 0,
            },
        )
        .account_override_slot(&pubkey1, config.account1_slot)
        .account_override_slot(&pubkey2, config.account2_slot)
        .build();
    let (tx, rx) = mpsc::channel(1);
    let pubsub_client = ChainPubsubClientMock::new(tx, rx);

    let (forward_tx, forward_rx) = mpsc::channel(100);
    let (subscribed_accounts, config) = create_test_lru_cache(1000);
    let chain_slot = Arc::<AtomicU64>::default();

    (
        RemoteAccountProvider::new(
            rpc_client,
            pubsub_client,
            forward_tx,
            &config,
            subscribed_accounts,
            ChainSlot::new(chain_slot),
        )
        .await
        .unwrap(),
        forward_rx,
    )
}

#[tokio::test]
async fn test_classification_placeholder_cleanup_is_generation_scoped() {
    let pubkey = solana_pubkey::Pubkey::new_unique();
    let subscription_ownership: SubscriptionOwnershipMap = Default::default();
    let subscription_transition_lock = Arc::new(AsyncMutex::new(()));
    let placeholder = SubscriptionOwnership {
        classification_placeholder_generation: Some(2),
        ..Default::default()
    };
    subscription_ownership
        .lock()
        .await
        .insert(pubkey, placeholder);

    let (primary, _) = create_test_lru_cache(10);
    let (secondary, _) = create_test_lru_cache(10);
    cleanup_classification_placeholders(
        &subscription_ownership,
        &subscription_transition_lock,
        &primary,
        &secondary,
        &HashMap::from([(pubkey, 1)]),
    )
    .await;
    assert!(subscription_ownership.lock().await.contains_key(&pubkey));

    cleanup_classification_placeholders(
        &subscription_ownership,
        &subscription_transition_lock,
        &primary,
        &secondary,
        &HashMap::from([(pubkey, 2)]),
    )
    .await;
    assert!(!subscription_ownership.lock().await.contains_key(&pubkey));
}

#[tokio::test]
async fn test_cancelled_subscription_setup_cleans_classification_placeholder() {
    let pubkey = solana_pubkey::Pubkey::new_unique();
    let generation = 1;
    let (sender, receiver) = oneshot::channel();
    let fetching_accounts = Arc::new(Mutex::new(HashMap::from([(
        pubkey,
        FetchingAccountState {
            generation,
            fetch_start_slot: 0,
            fetch_context: AccountFetchContext::rpc_get_account(),
            requires_full_coverage: Arc::new(
                PendingFetchCoverageRequirement::new(false),
            ),
            owner_started_at: std::time::Instant::now(),
            waiters: vec![sender],
        },
    )])));
    let subscription_ownership: SubscriptionOwnershipMap = Default::default();
    let subscription_transition_lock = Arc::new(AsyncMutex::new(()));
    let placeholder = SubscriptionOwnership {
        classification_placeholder_generation: Some(generation),
        ..Default::default()
    };
    subscription_ownership
        .lock()
        .await
        .insert(pubkey, placeholder);

    let transition_guard = subscription_transition_lock.lock().await;
    let guard = ClaimedSubscriptionSetupGuard::new(
        fetching_accounts.clone(),
        subscription_ownership.clone(),
        subscription_transition_lock.clone(),
        create_test_lru_cache(10).0,
        create_test_lru_cache(10).0,
        vec![pubkey],
        HashMap::from([(pubkey, generation)]),
    );
    drop(guard);

    assert!(!fetching_accounts.lock().unwrap().contains_key(&pubkey));
    assert!(receiver.await.unwrap().is_err());
    assert!(subscription_ownership.lock().await.contains_key(&pubkey));
    drop(transition_guard);

    tokio::time::timeout(Duration::from_secs(1), async {
        while subscription_ownership.lock().await.contains_key(&pubkey) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("classification placeholder cleanup should complete");
}

#[tokio::test]
async fn test_failed_placeholder_adoption_preserves_generation_for_cleanup() {
    let pubkey = solana_pubkey::Pubkey::new_unique();
    let account = Account {
        lamports: 1_000_000,
        data: vec![],
        owner: system_program::id(),
        executable: false,
        rent_epoch: 0,
    };
    let ProviderTestCtx {
        provider,
        pubsub_client,
        _forward_rx,
        ..
    } = setup_provider(pubkey, account).await;
    let generation = 1;
    let placeholder = SubscriptionOwnership {
        classification_placeholder_generation: Some(generation),
        ..Default::default()
    };
    provider
        .subscription_ownership
        .lock()
        .await
        .insert(pubkey, placeholder);

    pubsub_client.simulate_disconnect();
    assert!(provider
        .acquire_subscription_with_origin(
            &pubkey,
            SubscriptionReason::DirectAccount,
            SubscriptionRegistrationOrigin::Fetch(
                AccountFetchContext::rpc_get_account(),
            ),
        )
        .await
        .is_err());

    let ownership = provider.subscription_ownership.lock().await;
    let placeholder = ownership
        .get(&pubkey)
        .expect("failed adoption should retain the placeholder for cleanup");
    assert!(placeholder.is_empty());
    assert_eq!(
        placeholder.classification_placeholder_generation,
        Some(generation)
    );
    drop(ownership);

    cleanup_classification_placeholders(
        &provider.subscription_ownership,
        &provider.subscription_transition_lock,
        &provider.lrucache_subscribed_accounts,
        &provider.secondary_subscriptions,
        &HashMap::from([(pubkey, generation)]),
    )
    .await;
    assert!(!provider
        .subscription_ownership
        .lock()
        .await
        .contains_key(&pubkey));
}

#[tokio::test]
async fn test_companion_fetch_metrics_record_fast_path_success() {
    let _metrics_guard =
        crate::testing::pending_metric_test_lock().lock().await;
    const CURRENT_SLOT: u64 = 42;
    let pubkey1 = random_pubkey();
    let pubkey2 = random_pubkey();
    let (remote_account_provider, _) = setup_matching_slots(
        TestSlotConfig {
            current_slot: CURRENT_SLOT,
            account1_slot: CURRENT_SLOT,
            account2_slot: CURRENT_SLOT,
        },
        pubkey1,
        pubkey2,
    )
    .await;
    let context = AccountFetchContext::subscription_update(
        AccountFetchReason::ProgramData,
    );
    let kind = ChainlinkCompanionFetchKind::ProgramData;
    let outcome = ChainlinkCompanionFetchOutcome::Succeeded;
    let attempts_count_before =
        chainlink_companion_fetch_attempts_sample_count(context, kind, outcome);
    let attempts_sum_before =
        chainlink_companion_fetch_attempts_sample_sum(context, kind, outcome);
    let duration_count_before =
        chainlink_companion_fetch_duration_sample_count(context, kind, outcome);
    let duration_sum_before =
        chainlink_companion_fetch_duration_sample_sum(context, kind, outcome);

    let res = remote_account_provider
        .try_get_multi_until_slots_match(
            &[pubkey1, pubkey2],
            Some(MatchSlotsConfig {
                max_retries: 10,
                retry_interval_ms: 50,
                min_context_slot: Some(CURRENT_SLOT),
                companion_fetch_kind: kind,
            }),
            context,
        )
        .await;

    assert!(res.is_ok());
    assert_eq!(
        chainlink_companion_fetch_attempts_sample_count(context, kind, outcome),
        attempts_count_before + 1
    );
    assert_eq!(
        chainlink_companion_fetch_attempts_sample_sum(context, kind, outcome),
        attempts_sum_before + 1.0
    );
    assert_eq!(
        chainlink_companion_fetch_duration_sample_count(context, kind, outcome),
        duration_count_before + 1
    );
    assert!(
        chainlink_companion_fetch_duration_sample_sum(context, kind, outcome)
            >= duration_sum_before
    );
}

#[tokio::test]
async fn test_companion_fetch_metrics_record_retry_success() {
    let _metrics_guard =
        crate::testing::pending_metric_test_lock().lock().await;
    const CURRENT_SLOT: u64 = 42;
    let pubkey1 = random_pubkey();
    let pubkey2 = random_pubkey();
    let (remote_account_provider, _) = setup_matching_slots(
        TestSlotConfig {
            current_slot: CURRENT_SLOT,
            account1_slot: CURRENT_SLOT,
            account2_slot: CURRENT_SLOT,
        },
        pubkey1,
        pubkey2,
    )
    .await;
    let rpc_to_advance = remote_account_provider.rpc_client.clone();
    let advance_handle = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        rpc_to_advance.set_slot(CURRENT_SLOT + 1);
    });
    let context = AccountFetchContext::subscription_update(
        AccountFetchReason::ProgramData,
    );
    let kind = ChainlinkCompanionFetchKind::ProgramData;
    let outcome = ChainlinkCompanionFetchOutcome::Succeeded;
    let attempts_count_before =
        chainlink_companion_fetch_attempts_sample_count(context, kind, outcome);
    let attempts_sum_before =
        chainlink_companion_fetch_attempts_sample_sum(context, kind, outcome);

    let res = remote_account_provider
        .try_get_multi_until_slots_match(
            &[pubkey1, pubkey2],
            Some(MatchSlotsConfig {
                max_retries: 20,
                retry_interval_ms: 10,
                min_context_slot: Some(CURRENT_SLOT + 1),
                companion_fetch_kind: kind,
            }),
            context,
        )
        .await;
    advance_handle.await.unwrap();

    assert!(res.is_ok());
    assert_eq!(
        chainlink_companion_fetch_attempts_sample_count(context, kind, outcome),
        attempts_count_before + 1
    );
    assert!(
        chainlink_companion_fetch_attempts_sample_sum(context, kind, outcome)
            > attempts_sum_before + 1.0
    );
}

#[tokio::test]
async fn test_companion_fetch_metrics_record_slot_mismatch_failure() {
    let _metrics_guard =
        crate::testing::pending_metric_test_lock().lock().await;
    let context = AccountFetchContext::rpc_get_account()
        .with_reason(AccountFetchReason::DelegationRecord);
    let kind = ChainlinkCompanionFetchKind::DelegationRecord;
    let outcome = ChainlinkCompanionFetchOutcome::FailedSlotMismatch;
    let attempts_count_before =
        chainlink_companion_fetch_attempts_sample_count(context, kind, outcome);
    let duration_count_before =
        chainlink_companion_fetch_duration_sample_count(context, kind, outcome);

    // RPC-only retries in the provider test mock use one batch context slot,
    // which normalizes slots before the terminal mismatch branch. Exercise the
    // private observation helper directly so this test covers the metric path
    // without changing production retry behavior.
    observe_companion_fetch_if_configured(
        context,
        Some(kind),
        outcome,
        1,
        std::time::Instant::now(),
    );

    assert_eq!(
        chainlink_companion_fetch_attempts_sample_count(context, kind, outcome),
        attempts_count_before + 1
    );
    assert_eq!(
        chainlink_companion_fetch_duration_sample_count(context, kind, outcome),
        duration_count_before + 1
    );
}

#[tokio::test]
async fn test_companion_fetch_metrics_not_recorded_without_kind() {
    let _metrics_guard =
        crate::testing::pending_metric_test_lock().lock().await;
    const CURRENT_SLOT: u64 = 42;
    let pubkey1 = random_pubkey();
    let pubkey2 = random_pubkey();
    let (remote_account_provider, _) = setup_matching_slots(
        TestSlotConfig {
            current_slot: CURRENT_SLOT,
            account1_slot: CURRENT_SLOT,
            account2_slot: CURRENT_SLOT,
        },
        pubkey1,
        pubkey2,
    )
    .await;
    let context = AccountFetchContext::project_ata();
    let kind = ChainlinkCompanionFetchKind::AtaProjection;
    let outcome = ChainlinkCompanionFetchOutcome::Succeeded;
    let attempts_count_before =
        chainlink_companion_fetch_attempts_sample_count(context, kind, outcome);
    let duration_count_before =
        chainlink_companion_fetch_duration_sample_count(context, kind, outcome);

    let res = remote_account_provider
        .try_get_multi_until_slots_match(&[pubkey1, pubkey2], None, context)
        .await;

    assert!(res.is_ok());
    assert_eq!(
        chainlink_companion_fetch_attempts_sample_count(context, kind, outcome),
        attempts_count_before
    );
    assert_eq!(
        chainlink_companion_fetch_duration_sample_count(context, kind, outcome),
        duration_count_before
    );
}

#[tokio::test]
async fn test_try_get_multi_setup_subscriptions_failure_cleans_up_pending_entry(
) {
    let _metrics_guard =
        crate::testing::pending_metric_test_lock().lock().await;
    let pubkey = solana_pubkey::Pubkey::new_unique();
    let account = Account {
        lamports: 1_000_000,
        data: vec![1, 2, 3, 4],
        owner: solana_pubkey::Pubkey::new_unique(),
        executable: false,
        rent_epoch: 0,
    };

    let ProviderTestCtx {
        provider,
        pubsub_client,
        _forward_rx,
        ..
    } = setup_provider(pubkey, account).await;

    pubsub_client.block_subscribe();

    let task_handle = tokio::spawn({
        let provider = provider.clone();
        async move {
            provider
                .try_get_multi(
                    &[pubkey],
                    None,
                    AccountFetchContext::rpc_get_account(),
                    None,
                )
                .await
        }
    });

    pubsub_client.wait_for_subscribe_attempts(1).await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(provider.is_pending(&pubkey));

    pubsub_client.simulate_disconnect();
    pubsub_client.release_subscribe();

    let result = tokio::time::timeout(Duration::from_secs(2), task_handle)
        .await
        .expect("owner task should complete")
        .expect("owner task should not panic");
    let err = result.expect_err("setup_subscriptions should fail");
    assert!(matches!(
        err,
        RemoteAccountProviderError::AccountSubscriptionsTaskFailed(_)
    ));
    assert!(!provider.is_pending(&pubkey));

    pubsub_client.try_reconnect().await.unwrap();
    let retry = provider
        .try_get_multi(
            &[pubkey],
            None,
            AccountFetchContext::rpc_get_account(),
            None,
        )
        .await
        .expect("retry after cleanup should succeed");
    assert_eq!(retry.len(), 1);
}

#[tokio::test]
async fn test_try_get_multi_waiter_receives_setup_subscriptions_failure() {
    let _metrics_guard =
        crate::testing::pending_metric_test_lock().lock().await;
    let pubkey = solana_pubkey::Pubkey::new_unique();
    let account = Account {
        lamports: 1_000_000,
        data: vec![1, 2, 3, 4],
        owner: solana_pubkey::Pubkey::new_unique(),
        executable: false,
        rent_epoch: 0,
    };

    let ProviderTestCtx {
        provider,
        pubsub_client,
        _forward_rx,
        ..
    } = setup_provider(pubkey, account).await;

    pubsub_client.block_subscribe();

    let first_task_handle = tokio::spawn({
        let provider = provider.clone();
        async move {
            provider
                .try_get_multi(
                    &[pubkey],
                    None,
                    AccountFetchContext::rpc_get_account(),
                    None,
                )
                .await
        }
    });

    pubsub_client.wait_for_subscribe_attempts(1).await;
    tokio::time::sleep(Duration::from_millis(50)).await;

    let second_task_handle = tokio::spawn({
        let provider = provider.clone();
        async move {
            provider
                .try_get_multi(
                    &[pubkey],
                    None,
                    AccountFetchContext::rpc_get_account(),
                    None,
                )
                .await
        }
    });

    let waiter_registration_start = tokio::time::Instant::now();
    let waiter_registration_timeout = Duration::from_secs(2);
    loop {
        let waiter_count = {
            let fetching = provider.fetching_accounts.lock().unwrap();
            fetching.get(&pubkey).map(|s| s.waiters.len()).unwrap_or(0)
        };
        if waiter_count >= 2 {
            break;
        }
        assert!(
            waiter_registration_start.elapsed() < waiter_registration_timeout,
            "second_task_handle did not register as a waiter in \
             provider.fetching_accounts for {pubkey} within \
             {waiter_registration_timeout:?}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    pubsub_client.simulate_disconnect();
    pubsub_client.release_subscribe();

    let first_result =
        tokio::time::timeout(Duration::from_secs(2), first_task_handle)
            .await
            .expect("owner task should complete")
            .expect("owner task should not panic");
    let second_result =
        tokio::time::timeout(Duration::from_secs(2), second_task_handle)
            .await
            .expect("waiter task should complete")
            .expect("waiter task should not panic");

    let first_err = first_result.expect_err("owner should fail");
    let second_err = second_result.expect_err("waiter should fail");
    assert!(matches!(
        first_err,
        RemoteAccountProviderError::AccountSubscriptionsTaskFailed(_)
    ));
    assert!(matches!(
        second_err,
        RemoteAccountProviderError::AccountResolutionsFailed(_)
    ));
    assert!(!provider.is_pending(&pubkey));
}

#[tokio::test]
async fn test_ensure_subscription_does_not_duplicate_existing_reason() {
    let pubkey = solana_pubkey::Pubkey::new_unique();
    let account = Account {
        lamports: 1_000_000,
        data: vec![],
        owner: system_program::id(),
        executable: false,
        rent_epoch: 0,
    };
    let ProviderTestCtx { provider, .. } =
        setup_provider(pubkey, account).await;

    provider
        .ensure_subscription(&pubkey, SubscriptionReason::AtaProjection)
        .await
        .unwrap();
    provider
        .ensure_subscription(&pubkey, SubscriptionReason::AtaProjection)
        .await
        .unwrap();

    let unsubscribed = provider
        .release_single_subscription(&pubkey, SubscriptionReason::AtaProjection)
        .await
        .unwrap();

    assert!(unsubscribed);
    assert!(!provider.is_watching(&pubkey));
}

#[tokio::test]
async fn test_release_subscription_reason_keeps_watching_until_last_direct_refcount(
) {
    let pubkey = solana_pubkey::Pubkey::new_unique();
    let account = Account {
        lamports: 1_000_000,
        data: vec![1, 2, 3, 4],
        owner: solana_pubkey::Pubkey::new_unique(),
        executable: false,
        rent_epoch: 0,
    };

    let ProviderTestCtx {
        provider,
        pubsub_client,
        _forward_rx,
        ..
    } = setup_provider(pubkey, account).await;

    provider
        .acquire_subscription(&pubkey, SubscriptionReason::DirectAccount)
        .await
        .unwrap();
    provider
        .acquire_subscription(&pubkey, SubscriptionReason::DirectAccount)
        .await
        .unwrap();

    let unsubscribed = provider
        .release_single_subscription(&pubkey, SubscriptionReason::DirectAccount)
        .await
        .unwrap();

    assert!(!unsubscribed);
    assert!(provider.is_watching(&pubkey));
    assert!(pubsub_client.subscriptions_union().contains(&pubkey));

    let unsubscribed = provider
        .release_single_subscription(&pubkey, SubscriptionReason::DirectAccount)
        .await
        .unwrap();

    assert!(unsubscribed);
    assert!(!provider.is_watching(&pubkey));
    assert!(!pubsub_client.subscriptions_union().contains(&pubkey));
}

#[tokio::test]
async fn test_release_subscription_reason_all_clears_duplicate_reason_counts() {
    let pubkey = solana_pubkey::Pubkey::new_unique();
    let account = Account {
        lamports: 1_000_000,
        data: vec![],
        owner: solana_pubkey::Pubkey::new_unique(),
        executable: false,
        rent_epoch: 0,
    };

    let ProviderTestCtx {
        provider,
        _forward_rx,
        ..
    } = setup_provider(pubkey, account).await;

    provider
        .acquire_subscription(&pubkey, SubscriptionReason::DirectAccount)
        .await
        .unwrap();
    provider
        .acquire_subscription(&pubkey, SubscriptionReason::DirectAccount)
        .await
        .unwrap();
    provider
        .acquire_subscription(&pubkey, SubscriptionReason::UndelegationTracking)
        .await
        .unwrap();

    assert!(provider.is_watching(&pubkey));

    let unsubscribed = provider
        .release_subscription_with_mode(
            &pubkey,
            SubscriptionReason::DirectAccount,
            SubscriptionReleaseMode::All,
        )
        .await
        .unwrap();

    assert!(!unsubscribed);
    assert!(provider.is_watching(&pubkey));

    let unsubscribed = provider
        .release_subscription_with_mode(
            &pubkey,
            SubscriptionReason::UndelegationTracking,
            SubscriptionReleaseMode::All,
        )
        .await
        .unwrap();

    assert!(unsubscribed);
    assert!(!provider.is_watching(&pubkey));
}

#[tokio::test]
async fn test_release_subscription_reason_unsubscribes_after_final_release() {
    let pubkey = solana_pubkey::Pubkey::new_unique();
    let account = Account {
        lamports: 1_000_000,
        data: vec![1, 2, 3, 4],
        owner: solana_pubkey::Pubkey::new_unique(),
        executable: false,
        rent_epoch: 0,
    };

    let ProviderTestCtx {
        provider,
        pubsub_client,
        _forward_rx,
        ..
    } = setup_provider(pubkey, account).await;

    provider
        .acquire_subscription(&pubkey, SubscriptionReason::DirectAccount)
        .await
        .unwrap();

    let unsubscribed = provider
        .release_single_subscription(&pubkey, SubscriptionReason::DirectAccount)
        .await
        .unwrap();

    assert!(unsubscribed);
    assert!(!provider.is_watching(&pubkey));
    assert!(!pubsub_client.subscriptions_union().contains(&pubkey));
}

#[tokio::test]
async fn test_delegated_direct_cleanup_removes_final_direct_reason_without_notification(
) {
    let pubkey = solana_pubkey::Pubkey::new_unique();
    let account = Account {
        lamports: 1_000_000,
        data: vec![1, 2, 3, 4],
        owner: solana_pubkey::Pubkey::new_unique(),
        executable: false,
        rent_epoch: 0,
    };

    let ProviderTestCtx {
        provider,
        pubsub_client,
        _forward_rx,
        ..
    } = setup_provider(pubkey, account).await;
    let mut removed_rx = provider.try_get_removed_account_rx().unwrap();

    provider
        .acquire_subscription(&pubkey, SubscriptionReason::DirectAccount)
        .await
        .unwrap();

    let unsubscribed = provider
        .release_subscription_reason_silently_for_delegated_account(
            &pubkey,
            SubscriptionReason::DirectAccount,
        )
        .await
        .unwrap();

    assert!(unsubscribed);
    assert!(!provider.is_watching(&pubkey));
    assert!(!pubsub_client.subscriptions_union().contains(&pubkey));
    assert!(matches!(
        removed_rx.try_recv(),
        Err(tokio::sync::mpsc::error::TryRecvError::Empty)
    ));
}

#[tokio::test]
async fn test_releasing_absent_reason_preserves_existing_subscription() {
    let pubkey = Pubkey::new_unique();
    let ctx = setup_provider(pubkey, Account::default()).await;
    ctx.provider.abort_subscription_reconciler_for_test().await;

    ctx.provider
        .acquire_subscription(&pubkey, SubscriptionReason::DirectAccount)
        .await
        .unwrap();
    let unsubscribe_attempts = ctx.pubsub_client.unsubscribe_attempts();

    let unsubscribed = ctx
        .provider
        .release_single_subscription(
            &pubkey,
            SubscriptionReason::DelegationRecord,
        )
        .await
        .unwrap();

    assert!(!unsubscribed);
    assert_eq!(
        ctx.pubsub_client.unsubscribe_attempts(),
        unsubscribe_attempts
    );
    assert!(ctx.provider.is_watching(&pubkey));
    assert!(ctx.pubsub_client.subscriptions_union().contains(&pubkey));
    assert!(
        ctx.provider
            .has_subscription_reason(&pubkey, SubscriptionReason::DirectAccount)
            .await
    );
}

#[tokio::test]
async fn test_cancelled_final_release_restores_previous_authority() {
    #[derive(Clone, Copy, Debug)]
    enum ReleaseKind {
        Normal,
        Silent,
        Manual,
    }

    for release_kind in [
        ReleaseKind::Normal,
        ReleaseKind::Silent,
        ReleaseKind::Manual,
    ] {
        let pubkey = Pubkey::new_unique();
        let ctx = setup_provider(pubkey, Account::default()).await;
        ctx.provider.abort_subscription_reconciler_for_test().await;
        let mut removed_rx = ctx.provider.try_get_removed_account_rx().unwrap();
        drain_removed_account_rx(&mut removed_rx);

        ctx.provider
            .acquire_subscription(&pubkey, SubscriptionReason::DirectAccount)
            .await
            .unwrap();
        let subscribe_attempts = ctx.pubsub_client.subscribe_attempts();
        ctx.pubsub_client.pause_after_unsubscribe_remove();
        let removals_before = ctx.pubsub_client.unsubscribe_removals();

        let provider = ctx.provider.clone();
        let release = tokio::spawn(async move {
            match release_kind {
                ReleaseKind::Normal => provider
                    .release_single_subscription(
                        &pubkey,
                        SubscriptionReason::DirectAccount,
                    )
                    .await
                    .map(|_| ()),
                ReleaseKind::Silent => provider
                    .release_subscription_reason_silently_for_delegated_account(
                        &pubkey,
                        SubscriptionReason::DirectAccount,
                    )
                    .await
                    .map(|_| ()),
                ReleaseKind::Manual => provider.unsubscribe(&pubkey).await,
            }
        });
        ctx.pubsub_client
            .wait_for_unsubscribe_removals(removals_before + 1)
            .await;

        release.abort();
        assert!(
            release.await.unwrap_err().is_cancelled(),
            "{release_kind:?} caller should be cancelled"
        );

        ctx.pubsub_client.resume_after_unsubscribe_remove();
        wait_until("cancelled release to restore transport coverage", || {
            ctx.pubsub_client.is_subscribed(&pubkey)
                && ctx.pubsub_client.subscribe_attempts() > subscribe_attempts
        })
        .await;

        assert!(ctx.provider.is_watching(&pubkey));
        assert!(ctx
            .provider
            .subscription_ownership
            .lock()
            .await
            .contains_key(&pubkey));
        assert!(ctx.pubsub_client.subscriptions_union().contains(&pubkey));
        assert!(matches!(
            removed_rx.try_recv(),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty)
        ));
    }
}

#[tokio::test]
async fn test_delegated_direct_cleanup_keeps_undelegation_tracking() {
    let pubkey = solana_pubkey::Pubkey::new_unique();
    let account = Account {
        lamports: 1_000_000,
        data: vec![1, 2, 3, 4],
        owner: solana_pubkey::Pubkey::new_unique(),
        executable: false,
        rent_epoch: 0,
    };

    let ProviderTestCtx {
        provider,
        pubsub_client,
        _forward_rx,
        ..
    } = setup_provider(pubkey, account).await;

    provider
        .acquire_subscription(&pubkey, SubscriptionReason::DirectAccount)
        .await
        .unwrap();
    provider
        .acquire_subscription(&pubkey, SubscriptionReason::UndelegationTracking)
        .await
        .unwrap();

    let unsubscribed = provider
        .release_subscription_reason_silently_for_delegated_account(
            &pubkey,
            SubscriptionReason::DirectAccount,
        )
        .await
        .unwrap();

    assert!(!unsubscribed);
    assert!(provider.is_watching(&pubkey));
    assert!(pubsub_client.subscriptions_union().contains(&pubkey));

    let unsubscribed = provider
        .release_subscription_with_mode(
            &pubkey,
            SubscriptionReason::UndelegationTracking,
            SubscriptionReleaseMode::All,
        )
        .await
        .unwrap();

    assert!(unsubscribed);
    assert!(!provider.is_watching(&pubkey));
    assert!(!pubsub_client.subscriptions_union().contains(&pubkey));
}

#[tokio::test]
async fn test_subscription_reasons_do_not_release_each_other() {
    let pubkey = solana_pubkey::Pubkey::new_unique();
    let account = Account {
        lamports: 1_000_000,
        data: vec![1, 2, 3, 4],
        owner: solana_pubkey::Pubkey::new_unique(),
        executable: false,
        rent_epoch: 0,
    };

    let ProviderTestCtx {
        provider,
        pubsub_client,
        _forward_rx,
        ..
    } = setup_provider(pubkey, account).await;

    provider
        .acquire_subscription(&pubkey, SubscriptionReason::DirectAccount)
        .await
        .unwrap();
    provider
        .acquire_subscription(&pubkey, SubscriptionReason::DelegationRecord)
        .await
        .unwrap();

    let unsubscribed = provider
        .release_single_subscription(&pubkey, SubscriptionReason::DirectAccount)
        .await
        .unwrap();

    assert!(!unsubscribed);
    assert!(provider.is_watching(&pubkey));
    assert!(pubsub_client.subscriptions_union().contains(&pubkey));

    let unsubscribed = provider
        .release_single_subscription(
            &pubkey,
            SubscriptionReason::DelegationRecord,
        )
        .await
        .unwrap();

    assert!(unsubscribed);
    assert!(!provider.is_watching(&pubkey));
    assert!(!pubsub_client.subscriptions_union().contains(&pubkey));
}

#[tokio::test]
async fn test_concurrent_reason_changes_do_not_unsubscribe_until_final_release()
{
    let pubkey = solana_pubkey::Pubkey::new_unique();
    let account = Account {
        lamports: 1_000_000,
        data: vec![1, 2, 3, 4],
        owner: solana_pubkey::Pubkey::new_unique(),
        executable: false,
        rent_epoch: 0,
    };

    let ProviderTestCtx {
        provider,
        pubsub_client,
        _forward_rx,
        ..
    } = setup_provider(pubkey, account).await;

    provider
        .acquire_subscription(&pubkey, SubscriptionReason::DirectAccount)
        .await
        .unwrap();

    let (acquire_result, release_result) = tokio::join!(
        provider.acquire_subscription(
            &pubkey,
            SubscriptionReason::DelegationRecord,
        ),
        provider.release_single_subscription(
            &pubkey,
            SubscriptionReason::DirectAccount,
        )
    );
    acquire_result.unwrap();
    let unsubscribed = release_result.unwrap();

    assert!(!unsubscribed);
    assert!(provider.is_watching(&pubkey));
    assert!(pubsub_client.subscriptions_union().contains(&pubkey));

    let unsubscribed = provider
        .release_single_subscription(
            &pubkey,
            SubscriptionReason::DelegationRecord,
        )
        .await
        .unwrap();

    assert!(unsubscribed);
    assert!(!provider.is_watching(&pubkey));
    assert!(!pubsub_client.subscriptions_union().contains(&pubkey));
}

#[tokio::test]
async fn test_reconciler_does_not_unsubscribe_registration_between_pubsub_and_lru(
) {
    let pubkey = solana_pubkey::Pubkey::new_unique();
    let account = Account {
        lamports: 1_000_000,
        data: vec![1, 2, 3, 4],
        owner: solana_pubkey::Pubkey::new_unique(),
        executable: false,
        rent_epoch: 0,
    };

    let ProviderTestCtx {
        provider,
        pubsub_client,
        _forward_rx,
        ..
    } = setup_provider(pubkey, account).await;
    provider.abort_subscription_reconciler_for_test().await;

    pubsub_client.pause_after_subscribe_insert();
    let insertions_before = pubsub_client.subscribe_insertions();

    let provider_for_acquire = provider.clone();
    let acquire = tokio::spawn(async move {
        provider_for_acquire
            .acquire_subscription(&pubkey, SubscriptionReason::DirectAccount)
            .await
    });

    pubsub_client
        .wait_for_subscribe_insertions(insertions_before + 1)
        .await;

    assert!(pubsub_client.subscriptions_union().contains(&pubkey));
    assert!(!provider.is_watching(&pubkey));

    let provider_for_reconcile = provider.clone();
    let reconcile = tokio::spawn(async move {
        provider_for_reconcile
            .reconcile_subscriptions_once_for_test()
            .await
    });

    wait_until("reconciler to queue behind registration", || {
        provider.subscription_key_locks.registrations(&pubkey) == 2
    })
    .await;

    assert!(
        pubsub_client.subscriptions_union().contains(&pubkey),
        "reconciler must not unsubscribe a registration that is in pubsub but not yet in the LRU"
    );

    pubsub_client.resume_after_subscribe_insert();
    acquire
        .await
        .expect("acquire task should not panic")
        .expect("subscription acquire should succeed");
    reconcile.await.expect("reconcile task should not panic");

    assert!(provider.is_watching(&pubkey));
    assert!(pubsub_client.subscriptions_union().contains(&pubkey));
}

#[tokio::test]
async fn test_lock_aware_reconciler_still_removes_truly_stale_pubsub_only_subscription(
) {
    let setup_pubkey = solana_pubkey::Pubkey::new_unique();
    let stale_pubkey = solana_pubkey::Pubkey::new_unique();
    let account = Account {
        lamports: 1_000_000,
        data: vec![1, 2, 3, 4],
        owner: solana_pubkey::Pubkey::new_unique(),
        executable: false,
        rent_epoch: 0,
    };

    let ProviderTestCtx {
        provider,
        pubsub_client,
        _forward_rx,
        ..
    } = setup_provider(setup_pubkey, account).await;

    pubsub_client.insert_subscription(stale_pubkey);
    assert!(pubsub_client.subscriptions_union().contains(&stale_pubkey));
    assert!(!provider.is_watching(&stale_pubkey));

    provider.reconcile_subscriptions_once_for_test().await;

    assert!(!pubsub_client.subscriptions_union().contains(&stale_pubkey));
}

#[tokio::test]
async fn test_lock_aware_reconciler_still_resubscribes_lru_owned_missing_pubsub(
) {
    let pubkey = solana_pubkey::Pubkey::new_unique();
    let account = Account {
        lamports: 1_000_000,
        data: vec![1, 2, 3, 4],
        owner: solana_pubkey::Pubkey::new_unique(),
        executable: false,
        rent_epoch: 0,
    };

    let ProviderTestCtx {
        provider,
        pubsub_client,
        _forward_rx,
        ..
    } = setup_provider(pubkey, account).await;

    provider
        .acquire_subscription(&pubkey, SubscriptionReason::DirectAccount)
        .await
        .unwrap();
    assert!(provider.is_watching(&pubkey));
    assert!(pubsub_client.subscriptions_union().contains(&pubkey));

    pubsub_client
        .unsubscribe(pubkey)
        .await
        .expect("mock unsubscribe should remove pubsub state");
    assert!(provider.is_watching(&pubkey));
    assert!(!pubsub_client.subscriptions_union().contains(&pubkey));

    provider.reconcile_subscriptions_once_for_test().await;

    assert!(provider.is_watching(&pubkey));
    assert!(pubsub_client.subscriptions_union().contains(&pubkey));
}

#[tokio::test]
async fn test_lru_eviction_clears_all_subscription_reasons_for_evicted_pubkey()
{
    let pubkey1 = solana_pubkey::Pubkey::new_unique();
    let pubkey2 = solana_pubkey::Pubkey::new_unique();
    let account = Account {
        lamports: 1_000_000,
        data: vec![1, 2, 3, 4],
        owner: solana_pubkey::Pubkey::new_unique(),
        executable: false,
        rent_epoch: 0,
    };

    let ProviderTestCtx {
        provider,
        pubsub_client,
        _forward_rx,
        ..
    } = setup_provider_with_lru_capacity(pubkey1, account, 1).await;

    provider
        .acquire_subscription(&pubkey1, SubscriptionReason::DirectAccount)
        .await
        .unwrap();
    provider
        .acquire_subscription(&pubkey1, SubscriptionReason::DirectAccount)
        .await
        .unwrap();
    provider
        .acquire_subscription(&pubkey1, SubscriptionReason::DelegationRecord)
        .await
        .unwrap();

    assert!(provider.is_watching(&pubkey1));
    assert!(pubsub_client.subscriptions_union().contains(&pubkey1));
    assert!(provider
        .subscription_ownership
        .lock()
        .await
        .contains_key(&pubkey1));

    provider
        .acquire_subscription(&pubkey2, SubscriptionReason::DirectAccount)
        .await
        .unwrap();

    assert!(!provider.is_watching(&pubkey1));
    assert!(provider.is_watching(&pubkey2));
    assert!(pubsub_client.subscriptions_union().contains(&pubkey2));
    // The evicted key's unsubscribe and ownership cleanup run in a detached
    // task.
    wait_until("evicted account is unsubscribed", || {
        !pubsub_client.subscriptions_union().contains(&pubkey1)
    })
    .await;
    wait_until_ownership_removed(&provider, &pubkey1).await;
    assert!(provider
        .subscription_ownership
        .lock()
        .await
        .contains_key(&pubkey2));
}

#[tokio::test]
async fn test_lru_eviction_and_reason_release_are_serialized() {
    let pubkey1 = solana_pubkey::Pubkey::new_unique();
    let pubkey2 = solana_pubkey::Pubkey::new_unique();
    let account = Account {
        lamports: 1_000_000,
        data: vec![1, 2, 3, 4],
        owner: solana_pubkey::Pubkey::new_unique(),
        executable: false,
        rent_epoch: 0,
    };

    let ProviderTestCtx {
        provider,
        pubsub_client,
        _forward_rx,
        ..
    } = setup_provider_with_lru_capacity(pubkey1, account, 1).await;

    provider
        .acquire_subscription(&pubkey1, SubscriptionReason::DirectAccount)
        .await
        .unwrap();
    provider
        .acquire_subscription(&pubkey1, SubscriptionReason::DelegationRecord)
        .await
        .unwrap();

    let (acquire_result, release_result) = tokio::join!(
        provider
            .acquire_subscription(&pubkey2, SubscriptionReason::DirectAccount,),
        provider.release_single_subscription(
            &pubkey1,
            SubscriptionReason::DelegationRecord,
        )
    );

    acquire_result.unwrap();
    release_result.unwrap();

    assert!(!provider.is_watching(&pubkey1));
    assert!(provider.is_watching(&pubkey2));
    assert!(pubsub_client.subscriptions_union().contains(&pubkey2));
    // The evicted key's unsubscribe and ownership cleanup run in a detached
    // task.
    wait_until("evicted account is unsubscribed", || {
        !pubsub_client.subscriptions_union().contains(&pubkey1)
    })
    .await;
    wait_until_ownership_removed(&provider, &pubkey1).await;
    assert!(provider
        .subscription_ownership
        .lock()
        .await
        .contains_key(&pubkey2));
}

#[tokio::test]
async fn test_try_get_multi_owner_success_cleans_up_pending_entry() {
    let _metrics_guard =
        crate::testing::pending_metric_test_lock().lock().await;
    let pubkey = solana_pubkey::Pubkey::new_unique();
    let account = Account {
        lamports: 1_000_000,
        data: vec![1, 2, 3, 4],
        owner: solana_pubkey::Pubkey::new_unique(),
        executable: false,
        rent_epoch: 0,
    };

    let ProviderTestCtx {
        provider,
        rpc_client,
        _forward_rx,
        ..
    } = setup_provider(pubkey, account.clone()).await;

    rpc_client.block_fetches();
    let task_handle = tokio::spawn({
        let provider = provider.clone();
        async move {
            provider
                .try_get_multi(
                    &[pubkey],
                    None,
                    AccountFetchContext::rpc_get_account(),
                    None,
                )
                .await
        }
    });

    let pending_start = tokio::time::Instant::now();
    let pending_timeout = Duration::from_secs(2);
    loop {
        if provider.is_pending(&pubkey) {
            break;
        }
        assert!(
            pending_start.elapsed() < pending_timeout,
            "owner did not claim pending entry for {pubkey} within {pending_timeout:?}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    rpc_client.allow_fetches();

    let result = tokio::time::timeout(Duration::from_secs(2), task_handle)
        .await
        .expect("owner task should complete")
        .expect("owner task should not panic")
        .expect("fetch should succeed");
    assert_eq!(result.len(), 1);
    assert!(!provider.is_pending(&pubkey));
}

#[tokio::test]
async fn test_pending_fetch_metrics_count_remote_provider_owner_and_waiter() {
    let _metrics_guard =
        crate::testing::pending_metric_test_lock().lock().await;
    let pubkey = solana_pubkey::Pubkey::new_unique();
    let account = Account {
        lamports: 1_000_000,
        data: vec![1, 2, 3, 4],
        owner: solana_pubkey::Pubkey::new_unique(),
        executable: false,
        rent_epoch: 0,
    };

    let ProviderTestCtx {
        provider,
        rpc_client,
        _forward_rx,
        ..
    } = setup_provider(pubkey, account).await;

    let fetch_context = AccountFetchContext::rpc_get_multiple_accounts();
    let owned_baseline = pending_accounts_value(
        fetch_context,
        ChainlinkPendingFetchOutcome::Owned,
    );
    let joined_baseline = pending_accounts_value(
        fetch_context,
        ChainlinkPendingFetchOutcome::JoinedExisting,
    );
    let waiters_baseline = pending_waiters_value(fetch_context);

    rpc_client.block_fetches();

    let owner_task = tokio::spawn({
        let provider = provider.clone();
        async move {
            provider
                .try_get_multi(&[pubkey], None, fetch_context, None)
                .await
        }
    });

    wait_for_fetching_waiter_count(&provider, pubkey, 1).await;

    let waiter_task = tokio::spawn({
        let provider = provider.clone();
        async move {
            provider
                .try_get_multi(&[pubkey], None, fetch_context, None)
                .await
        }
    });

    wait_for_fetching_waiter_count(&provider, pubkey, 2).await;
    assert!(
        pending_waiters_gauge_value() >= 1,
        "remote provider waiter gauge should include this test's joined waiter"
    );

    rpc_client.allow_fetches();

    tokio::time::timeout(Duration::from_secs(2), owner_task)
        .await
        .expect("owner task should complete")
        .expect("owner task should not panic")
        .expect("owner fetch should succeed");
    tokio::time::timeout(Duration::from_secs(2), waiter_task)
        .await
        .expect("waiter task should complete")
        .expect("waiter task should not panic")
        .expect("waiter fetch should succeed");

    let owned_delta = pending_accounts_value(
        fetch_context,
        ChainlinkPendingFetchOutcome::Owned,
    )
    .saturating_sub(owned_baseline);
    assert!(
        owned_delta >= 1,
        "remote provider owned metric should increase by at least 1; got {owned_delta}"
    );
    let joined_delta = pending_accounts_value(
        fetch_context,
        ChainlinkPendingFetchOutcome::JoinedExisting,
    )
    .saturating_sub(joined_baseline);
    assert!(
        joined_delta >= 1,
        "remote provider joined-existing metric should increase by at least 1; got {joined_delta}"
    );
    let waiters_delta =
        pending_waiters_value(fetch_context).saturating_sub(waiters_baseline);
    assert!(
        waiters_delta >= 1,
        "remote provider waiter metric should increase by at least 1; got {waiters_delta}"
    );
}

#[tokio::test]
async fn test_pending_fetch_metrics_count_subscription_update_resolution_and_late_rpc(
) {
    let _metrics_guard =
        crate::testing::pending_metric_test_lock().lock().await;
    const CURRENT_SLOT: u64 = 100;
    let pubkey = solana_pubkey::Pubkey::new_unique();
    let account = Account {
        lamports: 1_000_000,
        data: vec![1, 2, 3, 4],
        owner: solana_pubkey::Pubkey::new_unique(),
        executable: false,
        rent_epoch: 0,
    };
    let subscription_account = Account {
        lamports: 2_000_000,
        ..account.clone()
    };

    let rpc_client = ChainRpcClientMockBuilder::new()
        .slot(CURRENT_SLOT)
        .clock_sysvar_for_slot(CURRENT_SLOT)
        .build();
    let (updates_sender, updates_receiver) = mpsc::channel(1_000);
    let pubsub_client =
        ChainPubsubClientMock::new(updates_sender, updates_receiver);
    let (forward_tx, _forward_rx) = mpsc::channel(1_000);
    let (subscribed_accounts, config) = create_test_lru_cache(1000);
    let provider = Arc::new(
        RemoteAccountProvider::new(
            rpc_client.clone(),
            pubsub_client.clone(),
            forward_tx,
            &config,
            subscribed_accounts,
            ChainSlot::new(Arc::<AtomicU64>::default()),
        )
        .await
        .unwrap(),
    );

    let fetch_context = AccountFetchContext::rpc_get_multiple_accounts();
    let resolved_baseline = pending_accounts_value(
        fetch_context,
        ChainlinkPendingFetchOutcome::ResolvedBySubscriptionUpdate,
    );
    let late_rpc_baseline = pending_accounts_value(
        fetch_context,
        ChainlinkPendingFetchOutcome::RpcFetchCompletedAfterUpdate,
    );

    rpc_client.block_fetches();

    let task_handle = tokio::spawn({
        let provider = provider.clone();
        async move {
            provider
                .try_get_multi(&[pubkey], None, fetch_context, None)
                .await
        }
    });

    wait_for_direct_subscription(&pubsub_client, pubkey).await;
    let fetch_start_slot = {
        let fetching = provider.fetching_accounts.lock().unwrap();
        fetching
            .get(&pubkey)
            .map(|state| state.fetch_start_slot)
            .expect("fetching account state should exist")
    };

    pubsub_client
        .send_account_update(pubkey, fetch_start_slot, &subscription_account)
        .await;

    let remote_accounts =
        tokio::time::timeout(Duration::from_secs(2), task_handle)
            .await
            .expect("subscription-resolved task should complete")
            .expect("subscription-resolved task should not panic")
            .expect("subscription-resolved fetch should succeed");
    assert_eq!(remote_accounts.len(), 1);
    assert_eq!(
        remote_accounts[0].source(),
        Some(RemoteAccountUpdateSource::Subscription)
    );
    let resolved_delta = pending_accounts_value(
        fetch_context,
        ChainlinkPendingFetchOutcome::ResolvedBySubscriptionUpdate,
    )
    .saturating_sub(resolved_baseline);
    assert!(
        resolved_delta >= 1,
        "remote provider subscription-resolution metric should increase by at least 1; got {resolved_delta}"
    );

    // A discarded result must not reclassify the account even when its
    // response slot is newer than the subscription update that won.
    rpc_client.set_current_slot(fetch_start_slot + 1);
    rpc_client.allow_fetches();
    wait_for_pending_account_delta_at_least(
        fetch_context,
        ChainlinkPendingFetchOutcome::RpcFetchCompletedAfterUpdate,
        late_rpc_baseline,
        1,
    )
    .await;
    let transition_guard = provider.subscription_transition_lock.lock().await;
    drop(transition_guard);
    assert!(provider.lrucache_subscribed_accounts.contains(&pubkey));
    assert!(!provider.secondary_subscriptions.contains(&pubkey));
}

#[tokio::test]
async fn test_get_non_existing_account() {
    init_logger();

    let remote_account_provider = {
        let (tx, rx) = mpsc::channel(1);
        let rpc_client = ChainRpcClientMockBuilder::new()
            .slot(1)
            .clock_sysvar_for_slot(1)
            .build();
        let pubsub_client =
            chain_pubsub_client::mock::ChainPubsubClientMock::new(tx, rx);
        let (fwd_tx, _fwd_rx) = mpsc::channel(100);
        let (subscribed_accounts, config) = create_test_lru_cache(1000);
        let chain_slot = Arc::<AtomicU64>::default();

        RemoteAccountProvider::new(
            rpc_client,
            pubsub_client,
            fwd_tx,
            &config,
            subscribed_accounts,
            ChainSlot::new(chain_slot),
        )
        .await
        .unwrap()
    };

    let pubkey = random_pubkey();
    let remote_account = remote_account_provider
        .try_get(pubkey, AccountFetchContext::rpc_get_account())
        .await
        .unwrap();
    assert!(!remote_account.is_found());
}

#[tokio::test]
async fn test_get_existing_account_for_valid_slot() {
    init_logger();

    const CURRENT_SLOT: u64 = 42;
    let pubkey = random_pubkey();

    let (remote_account_provider, rpc_client) = {
        let rpc_client = ChainRpcClientMockBuilder::new()
            .account(
                pubkey,
                Account {
                    lamports: 555,
                    data: vec![],
                    owner: system_program::id(),
                    executable: false,
                    rent_epoch: 0,
                },
            )
            .clock_sysvar_for_slot(CURRENT_SLOT)
            .slot(CURRENT_SLOT)
            .build();
        let (tx, rx) = mpsc::channel(1);
        let pubsub_client =
            chain_pubsub_client::mock::ChainPubsubClientMock::new(tx, rx);
        (
            {
                let (fwd_tx, _fwd_rx) = mpsc::channel(100);
                let (subscribed_accounts, config) = create_test_lru_cache(1000);
                let chain_slot = Arc::<AtomicU64>::default();

                RemoteAccountProvider::new(
                    rpc_client.clone(),
                    pubsub_client,
                    fwd_tx,
                    &config,
                    subscribed_accounts,
                    ChainSlot::new(chain_slot),
                )
                .await
                .unwrap()
            },
            rpc_client,
        )
    };

    let remote_account = remote_account_provider
        .try_get(pubkey, AccountFetchContext::rpc_get_account())
        .await
        .unwrap();
    let AccountAtSlot { account, slot } =
        rpc_client.get_account_at_slot(&pubkey).unwrap();
    assert_eq!(
        remote_account,
        RemoteAccount::from_fresh_account(
            account,
            slot,
            RemoteAccountUpdateSource::Fetch,
        )
    );
    assert_eq!(rpc_client.single_account_fetches(), 2);
    assert_eq!(rpc_client.multi_account_fetches(), 0);
}

#[tokio::test]
async fn test_get_accounts_until_slots_match_finding_matching_slot() {
    const CURRENT_SLOT: u64 = 42;
    let pubkey1 = random_pubkey();
    let pubkey2 = random_pubkey();
    let (remote_account_provider, _) = setup_matching_slots(
        TestSlotConfig {
            current_slot: CURRENT_SLOT,
            account1_slot: CURRENT_SLOT,
            account2_slot: CURRENT_SLOT + 1,
        },
        pubkey1,
        pubkey2,
    )
    .await;

    let remote_accounts = remote_account_provider
        .try_get_multi_until_slots_match(
            &[pubkey1, pubkey2],
            Some(MatchSlotsConfig {
                max_retries: 10,
                retry_interval_ms: 50,
                min_context_slot: None,
                companion_fetch_kind: ChainlinkCompanionFetchKind::ProgramData,
            }),
            AccountFetchContext::rpc_get_account(),
        )
        .await
        .unwrap();

    assert_eq!(remote_accounts.len(), 2);
    assert!(remote_accounts[0].is_found());
    assert!(remote_accounts[1].is_found());
    assert_eq!(remote_accounts[0].fresh_lamports(), Some(555));
    assert_eq!(remote_accounts[1].fresh_lamports(), Some(666));
}

#[tokio::test]
async fn test_get_accounts_until_slots_match_refetches_mixed_sources_as_rpc_batch(
) {
    const CURRENT_SLOT: u64 = 42;
    let pubkey1 = random_pubkey();
    let pubkey2 = random_pubkey();
    let account1 = Account {
        lamports: 555,
        data: vec![],
        owner: system_program::id(),
        executable: false,
        rent_epoch: 0,
    };
    let account2 = Account {
        lamports: 666,
        data: vec![],
        owner: system_program::id(),
        executable: false,
        rent_epoch: 0,
    };
    let subscription_account = Account {
        lamports: 777,
        ..account1.clone()
    };
    let rpc_client = ChainRpcClientMockBuilder::new()
        .slot(CURRENT_SLOT)
        .account(pubkey1, account1)
        .account(pubkey2, account2)
        .build();
    let (updates_tx, updates_rx) = mpsc::channel(100);
    let pubsub_client = ChainPubsubClientMock::new(updates_tx, updates_rx);
    let (forward_tx, _forward_rx) = mpsc::channel(100);
    let (subscribed_accounts, config) = create_test_lru_cache(1000);
    let provider = Arc::new(
        RemoteAccountProvider::new(
            rpc_client.clone(),
            pubsub_client.clone(),
            forward_tx,
            &config,
            subscribed_accounts,
            ChainSlot::new(Arc::<AtomicU64>::default()),
        )
        .await
        .unwrap(),
    );

    rpc_client.block_fetches();
    let task_handle = tokio::spawn({
        let provider = provider.clone();
        async move {
            provider
                .try_get_multi_until_slots_match(
                    &[pubkey1, pubkey2],
                    Some(MatchSlotsConfig {
                        max_retries: 3,
                        retry_interval_ms: 10,
                        min_context_slot: None,
                        companion_fetch_kind:
                            ChainlinkCompanionFetchKind::ProgramData,
                    }),
                    AccountFetchContext::rpc_get_account(),
                )
                .await
        }
    });

    let start = tokio::time::Instant::now();
    loop {
        let subscriptions = pubsub_client.subscriptions_union();
        if subscriptions.contains(&pubkey1) && subscriptions.contains(&pubkey2)
        {
            break;
        }
        assert!(start.elapsed() < Duration::from_secs(2));
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    pubsub_client
        .send_account_update(pubkey1, CURRENT_SLOT + 1, &subscription_account)
        .await;
    let start = tokio::time::Instant::now();
    loop {
        if !provider.is_pending(&pubkey1) && provider.is_pending(&pubkey2) {
            break;
        }
        assert!(start.elapsed() < Duration::from_secs(2));
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    rpc_client.allow_fetches();

    let remote_accounts =
        tokio::time::timeout(Duration::from_secs(2), task_handle)
            .await
            .expect("slot-match task should complete")
            .expect("slot-match task should not panic")
            .expect("slot-match fetch should succeed");

    assert_eq!(remote_accounts.len(), 2);
    assert_eq!(
        remote_accounts[0].source(),
        Some(RemoteAccountUpdateSource::Fetch)
    );
    assert_eq!(
        remote_accounts[1].source(),
        Some(RemoteAccountUpdateSource::Fetch)
    );
    assert_eq!(remote_accounts[0].slot(), CURRENT_SLOT);
    assert_eq!(remote_accounts[1].slot(), CURRENT_SLOT);
    assert_eq!(remote_accounts[0].fresh_lamports(), Some(555));
    assert_eq!(remote_accounts[1].fresh_lamports(), Some(666));
    assert_eq!(rpc_client.multi_account_fetches(), 2);
}

#[tokio::test]
async fn test_get_accounts_until_slots_match_not_finding_matching_slot() {
    const CURRENT_SLOT: u64 = 42;
    let pubkey1 = random_pubkey();
    let pubkey2 = random_pubkey();
    let (remote_account_provider, _) = setup_matching_slots(
        TestSlotConfig {
            current_slot: CURRENT_SLOT,
            account1_slot: CURRENT_SLOT,
            account2_slot: CURRENT_SLOT - 1,
        },
        pubkey1,
        pubkey2,
    )
    .await;

    let res = remote_account_provider
        .try_get_multi_until_slots_match(
            &[pubkey1, pubkey2],
            Some(MatchSlotsConfig {
                max_retries: 10,
                retry_interval_ms: 50,
                min_context_slot: None,
                companion_fetch_kind: ChainlinkCompanionFetchKind::ProgramData,
            }),
            AccountFetchContext::rpc_get_account(),
        )
        .await;

    debug!(result = ?res, "Result");
    assert!(res.is_ok());
    let accs = res.unwrap();

    assert_eq!(accs.len(), 2);
    assert!(accs[0].is_found());
    assert!(!accs[1].is_found());
}

#[tokio::test]
async fn test_get_accounts_until_slots_match_waits_when_chain_slot_smaller_than_min_context_slot(
) {
    const CURRENT_SLOT: u64 = 42;
    let pubkey1 = random_pubkey();
    let pubkey2 = random_pubkey();
    let (remote_account_provider, _) = setup_matching_slots(
        TestSlotConfig {
            current_slot: CURRENT_SLOT,
            account1_slot: CURRENT_SLOT,
            account2_slot: CURRENT_SLOT,
        },
        pubkey1,
        pubkey2,
    )
    .await;

    let rpc_to_advance = remote_account_provider.rpc_client.clone();
    let advance_handle = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(800)).await;
        rpc_to_advance.set_slot(CURRENT_SLOT + 1);
    });

    let remote_accounts = remote_account_provider
        .try_get_multi_until_slots_match(
            &[pubkey1, pubkey2],
            Some(MatchSlotsConfig {
                max_retries: 10,
                retry_interval_ms: 50,
                min_context_slot: Some(CURRENT_SLOT + 1),
                companion_fetch_kind: ChainlinkCompanionFetchKind::ProgramData,
            }),
            AccountFetchContext::rpc_get_account(),
        )
        .await
        .unwrap();

    advance_handle.await.unwrap();

    assert_eq!(remote_accounts.len(), 2);
    assert!(remote_accounts[0].is_found());
    assert!(remote_accounts[1].is_found());
    assert_eq!(remote_accounts[0].slot(), CURRENT_SLOT + 1);
    assert_eq!(remote_accounts[1].slot(), CURRENT_SLOT + 1);
}

#[tokio::test]
async fn test_slot_match_retry_reclassifies_found_account_to_primary() {
    const CURRENT_SLOT: u64 = 42;
    let missing = random_pubkey();
    let existing = random_pubkey();
    let account = Account {
        lamports: 1,
        ..Default::default()
    };
    let rpc_client = ChainRpcClientMockBuilder::new()
        .slot(CURRENT_SLOT)
        .clock_sysvar_for_slot(CURRENT_SLOT)
        .account(existing, account.clone())
        .build();
    let (updates_tx, updates_rx) = mpsc::channel(100);
    let pubsub_client = ChainPubsubClientMock::new(updates_tx, updates_rx);
    let (forward_tx, _forward_rx) = mpsc::channel(100);
    let (subscribed_accounts, config) = create_test_lru_cache(1000);
    let provider = Arc::new(
        RemoteAccountProvider::new(
            rpc_client.clone(),
            pubsub_client,
            forward_tx,
            &config,
            subscribed_accounts,
            ChainSlot::new(Arc::<AtomicU64>::default()),
        )
        .await
        .unwrap(),
    );

    let rpc_to_advance = rpc_client.clone();
    let account_to_add = account.clone();
    let advance_handle = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        rpc_to_advance.add_account(missing, account_to_add);
        rpc_to_advance.set_slot(CURRENT_SLOT + 1);
    });

    let remote_accounts = provider
        .try_get_multi_until_slots_match(
            &[missing, existing],
            Some(MatchSlotsConfig {
                max_retries: 10,
                retry_interval_ms: 10,
                min_context_slot: Some(CURRENT_SLOT + 1),
                companion_fetch_kind: ChainlinkCompanionFetchKind::ProgramData,
            }),
            AccountFetchContext::rpc_get_account(),
        )
        .await
        .unwrap();
    advance_handle.await.unwrap();

    assert!(remote_accounts.iter().all(RemoteAccount::is_found));
    assert!(provider.lrucache_subscribed_accounts.contains(&missing));
    assert!(!provider.secondary_subscriptions.contains(&missing));
}

#[tokio::test]
async fn test_get_accounts_until_slots_match_finding_matching_slot_but_one_account_slot_smaller_than_min_context_slot(
) {
    const CURRENT_SLOT: u64 = 42;
    let pubkey1 = random_pubkey();
    let pubkey2 = random_pubkey();
    let (remote_account_provider, _) = setup_matching_slots(
        TestSlotConfig {
            current_slot: CURRENT_SLOT,
            account1_slot: CURRENT_SLOT,
            account2_slot: CURRENT_SLOT - 1,
        },
        pubkey1,
        pubkey2,
    )
    .await;

    let res = remote_account_provider
        .try_get_multi_until_slots_match(
            &[pubkey1, pubkey2],
            Some(MatchSlotsConfig {
                max_retries: 10,
                retry_interval_ms: 50,
                min_context_slot: Some(CURRENT_SLOT),
                companion_fetch_kind: ChainlinkCompanionFetchKind::ProgramData,
            }),
            AccountFetchContext::rpc_get_account(),
        )
        .await;

    debug!(result = ?res, "Result");

    assert!(res.is_ok());
    let accs = res.unwrap();

    assert_eq!(accs.len(), 2);
    assert!(accs[0].is_found());
    assert!(!accs[1].is_found());
}

#[test]
fn test_match_slots_retry_delay_honors_configured_interval() {
    let config = MatchSlotsRetryConfig {
        max_retries: 10,
        retry_interval_ms: 50,
        min_context_slot: None,
    };

    assert_eq!(match_slots_retry_delay(&config), Duration::from_millis(50));
}

// -----------------
// LRU Cache/Eviction/Removal
// -----------------
async fn setup_with_accounts(
    pubkeys: &[Pubkey],
    accounts_capacity: usize,
) -> (
    RemoteAccountProvider<ChainRpcClientMock, ChainPubsubClientMock>,
    mpsc::Receiver<ForwardedSubscriptionUpdate>,
    mpsc::Receiver<Pubkey>,
) {
    let rpc_client = {
        let mut rpc_client_builder = ChainRpcClientMockBuilder::new().slot(1);
        for pubkey in pubkeys {
            rpc_client_builder = rpc_client_builder.account(
                *pubkey,
                Account {
                    lamports: 555,
                    data: vec![],
                    owner: system_program::id(),
                    executable: false,
                    rent_epoch: 0,
                },
            );
        }
        rpc_client_builder.build()
    };

    let (tx, rx) = mpsc::channel(1);
    let pubsub_client = ChainPubsubClientMock::new(tx, rx);

    let (forward_tx, forward_rx) = mpsc::channel(100);
    let (subscribed_accounts, config) =
        create_test_lru_cache(accounts_capacity);
    let chain_slot = Arc::<AtomicU64>::default();

    let provider = RemoteAccountProvider::new(
        rpc_client,
        pubsub_client,
        forward_tx,
        &config,
        subscribed_accounts,
        ChainSlot::new(chain_slot),
    )
    .await
    .unwrap();

    let removed_account_tx = provider.try_get_removed_account_rx().unwrap();
    (provider, forward_rx, removed_account_tx)
}

fn drain_removed_account_rx(rx: &mut mpsc::Receiver<Pubkey>) -> Vec<Pubkey> {
    let mut removed_accounts = Vec::new();
    while let Ok(pubkey) = rx.try_recv() {
        removed_accounts.push(pubkey);
    }
    removed_accounts
}

/// Awaits the next removal notification. Capacity-eviction cleanup runs as a
/// detached task, so its effects are asynchronous to the admission call.
async fn wait_for_removed_account(rx: &mut mpsc::Receiver<Pubkey>) -> Pubkey {
    tokio::time::timeout(Duration::from_secs(2), rx.recv())
        .await
        .expect("timed out waiting for removed account")
        .expect("removed account channel closed")
}

// Subscription lifecycle metric readers. Tests read the current counter value
// for one exact label tuple before and after an operation and compare the delta
// so they stay robust to global Prometheus counter state shared across runs.
fn registration_metric_value(
    origin: SubscriptionRegistrationOrigin,
    reason: SubscriptionReasonLabel,
    outcome: SubscriptionRegistrationOutcome,
) -> u64 {
    chainlink_subscription_registration_accounts_value(origin, reason, outcome)
}

fn release_metric_value(
    reason: SubscriptionReasonLabel,
    outcome: SubscriptionReleaseOutcome,
) -> u64 {
    chainlink_subscription_release_accounts_value(reason, outcome)
}

fn cleanup_metric_value(
    source: SubscriptionCleanupSource,
    outcome: SubscriptionCleanupOutcome,
) -> u64 {
    chainlink_subscription_cleanup_accounts_value(source, outcome)
}

static SUBSCRIPTION_LIFECYCLE_METRIC_TEST_GUARD: tokio::sync::Mutex<()> =
    tokio::sync::Mutex::const_new(());

#[tokio::test]
async fn test_add_accounts_up_to_limit_no_eviction() {
    // Higher level version (including removed_rx) from
    // src/remote_account_provider/lru_cache.rs:
    // - test_lru_cache_add_accounts_up_to_limit_no_eviction
    init_logger();

    let pubkey1 = Pubkey::new_unique();
    let pubkey2 = Pubkey::new_unique();
    let pubkey3 = Pubkey::new_unique();

    let pubkeys = &[pubkey1, pubkey2, pubkey3];

    let (provider, _, mut removed_rx) = setup_with_accounts(pubkeys, 3).await;

    // Add three accounts (up to limit)
    for pk in pubkeys {
        provider
            .try_get(*pk, AccountFetchContext::rpc_get_account())
            .await
            .unwrap();
    }

    // No evictions should occur
    let removed = drain_removed_account_rx(&mut removed_rx);
    debug!(removed = ?removed, "Removed accounts");
    assert!(removed.is_empty(), "Expected no removed accounts");
}

#[tokio::test]
async fn test_eviction_order() {
    // Higher level version (including removed_rx) from
    // src/remote_account_provider/lru_cache.rs:
    // - test_lru_cache_lru_eviction_order
    init_logger();

    let pubkey1 = Pubkey::new_unique();
    let pubkey2 = Pubkey::new_unique();
    let pubkey3 = Pubkey::new_unique();
    let pubkey4 = Pubkey::new_unique();
    let pubkey5 = Pubkey::new_unique();

    let pubkeys = &[pubkey1, pubkey2, pubkey3, pubkey4, pubkey5];
    let (provider, _, mut removed_rx) = setup_with_accounts(pubkeys, 3).await;

    // Fill cache: [1, 2, 3] (1 is least recently used)
    provider
        .try_get(pubkey1, AccountFetchContext::rpc_get_account())
        .await
        .unwrap();
    provider
        .try_get(pubkey2, AccountFetchContext::rpc_get_account())
        .await
        .unwrap();
    provider
        .try_get(pubkey3, AccountFetchContext::rpc_get_account())
        .await
        .unwrap();

    // Access pubkey1 to make it more recently used: [2, 3, 1]
    // This should just promote, making order [2, 3, 1]
    provider
        .try_get(pubkey1, AccountFetchContext::rpc_get_account())
        .await
        .unwrap();

    // Add pubkey4, should evict pubkey2 (now least recently used)
    provider
        .try_get(pubkey4, AccountFetchContext::rpc_get_account())
        .await
        .unwrap();

    // Check channel received the evicted account

    assert_eq!(wait_for_removed_account(&mut removed_rx).await, pubkey2);

    // Add pubkey5, should evict pubkey3 (now least recently used)
    provider
        .try_get(pubkey5, AccountFetchContext::rpc_get_account())
        .await
        .unwrap();

    // Check channel received the second evicted account
    assert_eq!(wait_for_removed_account(&mut removed_rx).await, pubkey3);
}

#[tokio::test]
async fn test_multiple_evictions_in_sequence() {
    // Higher level version (including removed_rx) from
    // src/remote_account_provider/lru_cache.rs:
    // - test_lru_cache_multiple_evictions_in_sequence
    init_logger();

    // Create test pubkeys
    let pubkeys: Vec<Pubkey> = (1..=7).map(|_| Pubkey::new_unique()).collect();

    let (provider, _, mut removed_rx) = setup_with_accounts(&pubkeys, 4).await;

    // Fill cache to capacity (no evictions)
    for pk in pubkeys.iter().take(4) {
        provider
            .try_get(*pk, AccountFetchContext::rpc_get_account())
            .await
            .unwrap();
    }

    // Add more accounts and verify evictions happen in LRU order
    for i in 4..7 {
        provider
            .try_get(pubkeys[i], AccountFetchContext::rpc_get_account())
            .await
            .unwrap();
        let expected_evicted = pubkeys[i - 4]; // Should evict the account added 4 steps ago

        // Verify the evicted account was sent over the channel
        assert_eq!(
            wait_for_removed_account(&mut removed_rx).await,
            expected_evicted
        );
    }
}

#[tokio::test]
async fn test_capacity_eviction_skips_undelegation_tracking_reason() {
    init_logger();
    let _metric_guard = SUBSCRIPTION_LIFECYCLE_METRIC_TEST_GUARD.lock().await;

    let pubkey1 = Pubkey::new_unique();
    let pubkey2 = Pubkey::new_unique();
    let pubkey3 = Pubkey::new_unique();
    let pubkeys = &[pubkey1, pubkey2, pubkey3];

    let (provider, _, mut removed_rx) = setup_with_accounts(pubkeys, 2).await;

    provider
        .acquire_subscription(&pubkey1, SubscriptionReason::DirectAccount)
        .await
        .unwrap();
    provider
        .acquire_subscription(
            &pubkey2,
            SubscriptionReason::UndelegationTracking,
        )
        .await
        .unwrap();

    let evicted_before = registration_metric_value(
        SubscriptionRegistrationOrigin::Internal,
        SubscriptionReasonLabel::DirectAccount,
        SubscriptionRegistrationOutcome::EvictedCandidate,
    );
    provider
        .acquire_subscription(&pubkey3, SubscriptionReason::DirectAccount)
        .await
        .unwrap();
    let evicted_after = registration_metric_value(
        SubscriptionRegistrationOrigin::Internal,
        SubscriptionReasonLabel::DirectAccount,
        SubscriptionRegistrationOutcome::EvictedCandidate,
    );
    assert_eq!(evicted_after - evicted_before, 1);

    assert!(!provider.is_watching(&pubkey1));
    assert!(provider.is_watching(&pubkey2));
    assert!(provider.is_watching(&pubkey3));
    // The evicted key's unsubscribe runs in a detached cleanup task.
    wait_until("evicted account is unsubscribed", || {
        !provider
            .pubsub_client()
            .subscriptions_union()
            .contains(&pubkey1)
    })
    .await;
    assert!(provider
        .pubsub_client()
        .subscriptions_union()
        .contains(&pubkey2));

    assert_eq!(wait_for_removed_account(&mut removed_rx).await, pubkey1);
}

#[tokio::test]
async fn test_capacity_eviction_unsubscribe_failure_keeps_admission() {
    init_logger();
    let _metric_guard = SUBSCRIPTION_LIFECYCLE_METRIC_TEST_GUARD.lock().await;

    let pubkey1 = Pubkey::new_unique();
    let pubkey2 = Pubkey::new_unique();
    let pubkeys = &[pubkey1, pubkey2];

    let (provider, _, mut removed_rx) = setup_with_accounts(pubkeys, 1).await;
    provider.abort_subscription_reconciler_for_test().await;

    provider
        .acquire_subscription(&pubkey1, SubscriptionReason::DirectAccount)
        .await
        .unwrap();
    provider.pubsub_client().fail_next_unsubscriptions(1);

    let evicted_before = registration_metric_value(
        SubscriptionRegistrationOrigin::Internal,
        SubscriptionReasonLabel::DirectAccount,
        SubscriptionRegistrationOutcome::EvictedCandidate,
    );
    let cleanup_before = cleanup_metric_value(
        SubscriptionCleanupSource::CapacityEviction,
        SubscriptionCleanupOutcome::UnsubscribeFailed,
    );

    // The admission stands even though the evicted key's unsubscribe fails;
    // the stray subscription is removed by the reconciler on a later pass.
    provider
        .acquire_subscription(&pubkey2, SubscriptionReason::DirectAccount)
        .await
        .unwrap();

    let evicted_after = registration_metric_value(
        SubscriptionRegistrationOrigin::Internal,
        SubscriptionReasonLabel::DirectAccount,
        SubscriptionRegistrationOutcome::EvictedCandidate,
    );
    assert_eq!(evicted_after - evicted_before, 1);

    assert!(!provider.is_watching(&pubkey1));
    assert!(provider.is_watching(&pubkey2));
    assert!(provider
        .pubsub_client()
        .subscriptions_union()
        .contains(&pubkey2));

    // Cleanup runs in a detached task: the unsubscribe failure is recorded,
    // ownership is dropped, and the removal notification still goes out.
    wait_until("evicted-cleanup unsubscribe failure is recorded", || {
        cleanup_metric_value(
            SubscriptionCleanupSource::CapacityEviction,
            SubscriptionCleanupOutcome::UnsubscribeFailed,
        ) - cleanup_before
            == 1
    })
    .await;
    assert_eq!(wait_for_removed_account(&mut removed_rx).await, pubkey1);
    assert!(!provider
        .subscription_ownership
        .lock()
        .await
        .contains_key(&pubkey1));
}

/// The detached evicted-cleanup task re-checks tier membership under the
/// evicted key's guard and must skip a key that was re-admitted while the
/// cleanup was waiting, leaving its subscription and ownership intact.
#[tokio::test]
async fn test_evicted_cleanup_skips_readmitted_account() {
    init_logger();
    let _metric_guard = SUBSCRIPTION_LIFECYCLE_METRIC_TEST_GUARD.lock().await;

    let pubkey1 = Pubkey::new_unique();
    let pubkey2 = Pubkey::new_unique();
    let pubkeys = &[pubkey1, pubkey2];

    let (provider, _, mut removed_rx) = setup_with_accounts(pubkeys, 1).await;

    provider
        .acquire_subscription(&pubkey1, SubscriptionReason::DirectAccount)
        .await
        .unwrap();

    // Hold pubkey1's per-key guard so the detached cleanup task spawned by
    // the eviction below blocks before its membership re-check.
    let guard = subscription_key_owned_guard_from_map(
        &provider.subscription_key_locks,
        pubkey1,
    )
    .await;

    let retained_before = cleanup_metric_value(
        SubscriptionCleanupSource::CapacityEviction,
        SubscriptionCleanupOutcome::RetainedIntentionally,
    );

    provider
        .acquire_subscription(&pubkey2, SubscriptionReason::DirectAccount)
        .await
        .unwrap();
    assert!(!provider.is_watching(&pubkey1));

    // Re-admit pubkey1 (as the secondary tier would after a fetch claims it)
    // before letting the cleanup task proceed.
    provider.secondary_subscriptions.add(pubkey1);
    drop(guard);

    wait_until("evicted cleanup skipped the re-admitted account", || {
        cleanup_metric_value(
            SubscriptionCleanupSource::CapacityEviction,
            SubscriptionCleanupOutcome::RetainedIntentionally,
        ) - retained_before
            == 1
    })
    .await;

    // The re-admitted key kept its subscription and ownership; no removal
    // notification was emitted for it.
    assert!(provider.is_watching(&pubkey1));
    assert!(provider
        .pubsub_client()
        .subscriptions_union()
        .contains(&pubkey1));
    assert!(provider
        .subscription_ownership
        .lock()
        .await
        .contains_key(&pubkey1));
    let removed_accounts = drain_removed_account_rx(&mut removed_rx);
    assert!(removed_accounts.is_empty());
}

#[tokio::test]
async fn test_capacity_eviction_missing_pubsub_subscription_completes_cleanup()
{
    init_logger();
    let _metric_guard = SUBSCRIPTION_LIFECYCLE_METRIC_TEST_GUARD.lock().await;

    let pubkey1 = Pubkey::new_unique();
    let pubkey2 = Pubkey::new_unique();
    let pubkeys = &[pubkey1, pubkey2];

    let (provider, _, mut removed_rx) = setup_with_accounts(pubkeys, 1).await;
    provider.abort_subscription_reconciler_for_test().await;

    provider
        .acquire_subscription(&pubkey1, SubscriptionReason::DirectAccount)
        .await
        .unwrap();
    provider.pubsub_client().remove_subscription(&pubkey1);

    let evicted_before = registration_metric_value(
        SubscriptionRegistrationOrigin::Internal,
        SubscriptionReasonLabel::DirectAccount,
        SubscriptionRegistrationOutcome::EvictedCandidate,
    );
    let error_before = registration_metric_value(
        SubscriptionRegistrationOrigin::Internal,
        SubscriptionReasonLabel::DirectAccount,
        SubscriptionRegistrationOutcome::UnsubscribeEvictedError,
    );
    let cleanup_before = cleanup_metric_value(
        SubscriptionCleanupSource::CapacityEviction,
        SubscriptionCleanupOutcome::AlreadyAbsent,
    );

    provider
        .acquire_subscription(&pubkey2, SubscriptionReason::DirectAccount)
        .await
        .unwrap();

    let evicted_after = registration_metric_value(
        SubscriptionRegistrationOrigin::Internal,
        SubscriptionReasonLabel::DirectAccount,
        SubscriptionRegistrationOutcome::EvictedCandidate,
    );
    let error_after = registration_metric_value(
        SubscriptionRegistrationOrigin::Internal,
        SubscriptionReasonLabel::DirectAccount,
        SubscriptionRegistrationOutcome::UnsubscribeEvictedError,
    );
    assert_eq!(evicted_after - evicted_before, 1);
    assert_eq!(error_after - error_before, 0);

    assert!(!provider.is_watching(&pubkey1));
    assert!(provider.is_watching(&pubkey2));
    assert!(provider
        .pubsub_client()
        .subscriptions_union()
        .contains(&pubkey2));

    // The evicted key's cleanup runs in a detached task.
    wait_until("evicted-cleanup already-absent outcome is recorded", || {
        cleanup_metric_value(
            SubscriptionCleanupSource::CapacityEviction,
            SubscriptionCleanupOutcome::AlreadyAbsent,
        ) - cleanup_before
            == 1
    })
    .await;
    assert!(!provider
        .pubsub_client()
        .subscriptions_union()
        .contains(&pubkey1));
    assert!(!provider
        .subscription_ownership
        .lock()
        .await
        .contains_key(&pubkey1));

    assert_eq!(wait_for_removed_account(&mut removed_rx).await, pubkey1);
}

#[tokio::test]
async fn test_capacity_eviction_all_protected_returns_error_without_unsubscribing_protected(
) {
    init_logger();
    let _metric_guard = SUBSCRIPTION_LIFECYCLE_METRIC_TEST_GUARD.lock().await;

    let pubkey1 = Pubkey::new_unique();
    let pubkey2 = Pubkey::new_unique();
    let pubkey3 = Pubkey::new_unique();
    let pubkeys = &[pubkey1, pubkey2, pubkey3];

    let (provider, _, mut removed_rx) = setup_with_accounts(pubkeys, 2).await;

    provider
        .acquire_subscription(
            &pubkey1,
            SubscriptionReason::UndelegationTracking,
        )
        .await
        .unwrap();
    provider
        .acquire_subscription(
            &pubkey2,
            SubscriptionReason::UndelegationTracking,
        )
        .await
        .unwrap();

    let registration_before = registration_metric_value(
        SubscriptionRegistrationOrigin::Internal,
        SubscriptionReasonLabel::DirectAccount,
        SubscriptionRegistrationOutcome::RejectedNoCapacity,
    );

    let err = provider
        .acquire_subscription(&pubkey3, SubscriptionReason::DirectAccount)
        .await
        .unwrap_err();

    let registration_after = registration_metric_value(
        SubscriptionRegistrationOrigin::Internal,
        SubscriptionReasonLabel::DirectAccount,
        SubscriptionRegistrationOutcome::RejectedNoCapacity,
    );
    assert_eq!(registration_after - registration_before, 1);

    assert!(matches!(
        err,
        RemoteAccountProviderError::NoEvictableSubscriptionCapacity { pubkey }
            if pubkey == pubkey3
    ));
    assert!(provider.is_watching(&pubkey1));
    assert!(provider.is_watching(&pubkey2));
    assert!(!provider.is_watching(&pubkey3));
    assert!(provider
        .pubsub_client()
        .subscriptions_union()
        .contains(&pubkey1));
    assert!(provider
        .pubsub_client()
        .subscriptions_union()
        .contains(&pubkey2));
    assert!(!provider
        .pubsub_client()
        .subscriptions_union()
        .contains(&pubkey3));

    let removed_accounts = drain_removed_account_rx(&mut removed_rx);
    assert!(removed_accounts.is_empty());
}

#[tokio::test]
async fn test_registration_metric_added_below_capacity() {
    init_logger();
    let _metric_guard = SUBSCRIPTION_LIFECYCLE_METRIC_TEST_GUARD.lock().await;

    let pubkey = solana_pubkey::Pubkey::new_unique();
    let account = Account {
        lamports: 1_000_000,
        data: vec![],
        owner: system_program::id(),
        executable: false,
        rent_epoch: 0,
    };
    let ProviderTestCtx { provider, .. } =
        setup_provider(pubkey, account).await;

    let before = registration_metric_value(
        SubscriptionRegistrationOrigin::Internal,
        SubscriptionReasonLabel::DirectAccount,
        SubscriptionRegistrationOutcome::AddedBelowCapacity,
    );
    provider
        .acquire_subscription(&pubkey, SubscriptionReason::DirectAccount)
        .await
        .unwrap();
    let after = registration_metric_value(
        SubscriptionRegistrationOrigin::Internal,
        SubscriptionReasonLabel::DirectAccount,
        SubscriptionRegistrationOutcome::AddedBelowCapacity,
    );
    assert_eq!(after - before, 1);
}

#[tokio::test]
async fn test_registration_metric_already_present_on_duplicate_acquire() {
    init_logger();
    let _metric_guard = SUBSCRIPTION_LIFECYCLE_METRIC_TEST_GUARD.lock().await;

    let pubkey = solana_pubkey::Pubkey::new_unique();
    let account = Account {
        lamports: 1_000_000,
        data: vec![],
        owner: system_program::id(),
        executable: false,
        rent_epoch: 0,
    };
    let ProviderTestCtx { provider, .. } =
        setup_provider(pubkey, account).await;

    provider
        .acquire_subscription(&pubkey, SubscriptionReason::DirectAccount)
        .await
        .unwrap();

    let before = registration_metric_value(
        SubscriptionRegistrationOrigin::Internal,
        SubscriptionReasonLabel::DirectAccount,
        SubscriptionRegistrationOutcome::AlreadyPresent,
    );
    provider
        .acquire_subscription(&pubkey, SubscriptionReason::DirectAccount)
        .await
        .unwrap();
    let after = registration_metric_value(
        SubscriptionRegistrationOrigin::Internal,
        SubscriptionReasonLabel::DirectAccount,
        SubscriptionRegistrationOutcome::AlreadyPresent,
    );
    assert_eq!(after - before, 1);
}

#[tokio::test]
async fn test_registration_metric_preserves_fetch_context() {
    init_logger();
    let _metric_guard = SUBSCRIPTION_LIFECYCLE_METRIC_TEST_GUARD.lock().await;

    let pubkey = solana_pubkey::Pubkey::new_unique();
    let account = Account {
        lamports: 1_000_000,
        data: vec![],
        owner: system_program::id(),
        executable: false,
        rent_epoch: 0,
    };
    let ProviderTestCtx { provider, .. } =
        setup_provider(pubkey, account).await;

    let before = registration_metric_value(
        SubscriptionRegistrationOrigin::Fetch(
            AccountFetchContext::rpc_get_account(),
        ),
        SubscriptionReasonLabel::DirectAccount,
        SubscriptionRegistrationOutcome::AddedBelowCapacity,
    );
    provider
        .try_get(pubkey, AccountFetchContext::rpc_get_account())
        .await
        .unwrap();
    let after = registration_metric_value(
        SubscriptionRegistrationOrigin::Fetch(
            AccountFetchContext::rpc_get_account(),
        ),
        SubscriptionReasonLabel::DirectAccount,
        SubscriptionRegistrationOutcome::AddedBelowCapacity,
    );
    assert_eq!(after - before, 1);
}

#[tokio::test]
async fn test_release_and_cleanup_metrics_on_successful_release() {
    init_logger();
    let _metric_guard = SUBSCRIPTION_LIFECYCLE_METRIC_TEST_GUARD.lock().await;

    let pubkey = solana_pubkey::Pubkey::new_unique();
    let account = Account {
        lamports: 1_000_000,
        data: vec![],
        owner: system_program::id(),
        executable: false,
        rent_epoch: 0,
    };
    let ProviderTestCtx { provider, .. } =
        setup_provider(pubkey, account).await;

    provider
        .acquire_subscription(&pubkey, SubscriptionReason::DirectAccount)
        .await
        .unwrap();

    let release_before = release_metric_value(
        SubscriptionReasonLabel::DirectAccount,
        SubscriptionReleaseOutcome::Unsubscribed,
    );
    let cleanup_before = cleanup_metric_value(
        SubscriptionCleanupSource::NormalRelease,
        SubscriptionCleanupOutcome::Unsubscribed,
    );
    let unsubscribed = provider
        .release_single_subscription(&pubkey, SubscriptionReason::DirectAccount)
        .await
        .unwrap();
    assert!(unsubscribed);
    let release_after = release_metric_value(
        SubscriptionReasonLabel::DirectAccount,
        SubscriptionReleaseOutcome::Unsubscribed,
    );
    let cleanup_after = cleanup_metric_value(
        SubscriptionCleanupSource::NormalRelease,
        SubscriptionCleanupOutcome::Unsubscribed,
    );
    assert_eq!(release_after - release_before, 1);
    assert_eq!(cleanup_after - cleanup_before, 1);
}

#[tokio::test]
async fn test_release_metric_already_absent() {
    init_logger();
    let _metric_guard = SUBSCRIPTION_LIFECYCLE_METRIC_TEST_GUARD.lock().await;

    let pubkey = solana_pubkey::Pubkey::new_unique();
    let account = Account {
        lamports: 1_000_000,
        data: vec![],
        owner: system_program::id(),
        executable: false,
        rent_epoch: 0,
    };
    let ProviderTestCtx { provider, .. } =
        setup_provider(pubkey, account).await;

    // A pubkey that was never subscribed has no ownership to release.
    let absent_pubkey = solana_pubkey::Pubkey::new_unique();
    let before = release_metric_value(
        SubscriptionReasonLabel::DirectAccount,
        SubscriptionReleaseOutcome::AlreadyAbsent,
    );
    let unsubscribed = provider
        .release_single_subscription(
            &absent_pubkey,
            SubscriptionReason::DirectAccount,
        )
        .await
        .unwrap();
    assert!(!unsubscribed);
    let after = release_metric_value(
        SubscriptionReasonLabel::DirectAccount,
        SubscriptionReleaseOutcome::AlreadyAbsent,
    );
    assert_eq!(after - before, 1);
}

#[tokio::test]
async fn test_cleanup_metric_on_manual_unsubscribe() {
    init_logger();
    let _metric_guard = SUBSCRIPTION_LIFECYCLE_METRIC_TEST_GUARD.lock().await;

    let pubkey = solana_pubkey::Pubkey::new_unique();
    let account = Account {
        lamports: 1_000_000,
        data: vec![],
        owner: system_program::id(),
        executable: false,
        rent_epoch: 0,
    };
    let ProviderTestCtx { provider, .. } =
        setup_provider(pubkey, account).await;

    provider
        .acquire_subscription(&pubkey, SubscriptionReason::DirectAccount)
        .await
        .unwrap();

    let unsub_before = cleanup_metric_value(
        SubscriptionCleanupSource::ManualUnsubscribe,
        SubscriptionCleanupOutcome::Unsubscribed,
    );
    provider.unsubscribe(&pubkey).await.unwrap();
    let unsub_after = cleanup_metric_value(
        SubscriptionCleanupSource::ManualUnsubscribe,
        SubscriptionCleanupOutcome::Unsubscribed,
    );
    assert_eq!(unsub_after - unsub_before, 1);

    // A second unsubscribe is a no-op because the pubkey already left the LRU.
    let absent_before = cleanup_metric_value(
        SubscriptionCleanupSource::ManualUnsubscribe,
        SubscriptionCleanupOutcome::AlreadyAbsent,
    );
    provider.unsubscribe(&pubkey).await.unwrap();
    let absent_after = cleanup_metric_value(
        SubscriptionCleanupSource::ManualUnsubscribe,
        SubscriptionCleanupOutcome::AlreadyAbsent,
    );
    assert_eq!(absent_after - absent_before, 1);
}

#[test]
fn test_removed_stuck_pubkey_symbols_are_absent_from_production_code() {
    // Audit command kept here for manual spot checks:
    // rg -n 'pending_request_guard|PendingRequestGuard|PendingRequestClaim|PendingRequestCompletion|claim_pending_request|finish_pending_request|PENDING_REQUEST_STALE_AFTER|PENDING_REQUEST_TIMEOUT|waiter_reconciliation_check|subscription_rollback_owners|try_unsubscribe_if_sole_owner|CancelStrategy|existing_subs|new_subs|is_pending\(&pubkey\)|FETCHING_ACCOUNT_STALE_AFTER|FetchingAccountGuard' magicblock-chainlink/src --glob '!**/tests.rs'
    fn visit_rs_files(dir: &Path, files: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).expect("read_dir should succeed") {
            let entry = entry.expect("dir entry should succeed");
            let path = entry.path();
            if path.is_dir() {
                visit_rs_files(&path, files);
            } else if path.extension().and_then(|ext| ext.to_str())
                == Some("rs")
                && path.file_name().and_then(|name| name.to_str())
                    != Some("tests.rs")
            {
                files.push(path);
            }
        }
    }

    fn is_ident_char(ch: char) -> bool {
        ch.is_ascii_alphanumeric() || ch == '_'
    }

    fn contains_ident(content: &str, ident: &str) -> bool {
        content.match_indices(ident).any(|(idx, _)| {
            let before = content[..idx].chars().next_back();
            let after = content[idx + ident.len()..].chars().next();
            !before.is_some_and(is_ident_char)
                && !after.is_some_and(is_ident_char)
        })
    }

    let ident_symbols = [
        "pending_request_guard",
        "PendingRequestGuard",
        "PendingRequestClaim",
        "PendingRequestCompletion",
        "claim_pending_request",
        "finish_pending_request",
        "PENDING_REQUEST_STALE_AFTER",
        "PENDING_REQUEST_TIMEOUT",
        "waiter_reconciliation_check",
        "subscription_rollback_owners",
        "try_unsubscribe_if_sole_owner",
        "CancelStrategy",
        "existing_subs",
        "new_subs",
        "FETCHING_ACCOUNT_STALE_AFTER",
        "FetchingAccountGuard",
    ];
    let exact_symbols = ["is_pending(&pubkey)"];

    let mut files = Vec::new();
    visit_rs_files(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
        &mut files,
    );

    let mut hits = Vec::new();
    for path in files {
        let content = std::fs::read_to_string(&path).unwrap_or_else(|err| {
            panic!("failed to read {}: {err}", path.display())
        });
        for symbol in ident_symbols {
            if contains_ident(&content, symbol) {
                hits.push(format!("{} contains {symbol}", path.display()));
            }
        }
        for symbol in exact_symbols {
            if content.contains(symbol) {
                hits.push(format!("{} contains {symbol}", path.display()));
            }
        }
    }

    assert!(
        hits.is_empty(),
        "forbidden production symbols remain:\n{}",
        hits.join("\n")
    );
}

impl<T: ChainRpcClient, U: ChainPubsubClient> RemoteAccountProvider<T, U> {
    /// Stops the background reconciler so its startup pass cannot race a
    /// test that injects pubsub failures or asserts on stray subscriptions.
    async fn abort_subscription_reconciler_for_test(&self) {
        if let Some(handle) = &self._active_subscriptions_task_handle {
            handle.abort();
            let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
            while !handle.is_finished() {
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "timed out stopping background subscription reconciler"
                );
                tokio::task::yield_now().await;
            }
        }
    }

    async fn reconcile_subscriptions_once_for_test(&self) -> usize {
        let never_evicted =
            self.lrucache_subscribed_accounts.never_evicted_accounts();
        subscription_reconciler::reconcile_subscriptions(
            &self.lrucache_subscribed_accounts,
            &self.secondary_subscriptions,
            &self.pubsub_client,
            &never_evicted,
            &self.removed_account_tx,
            Some(&self.subscription_key_locks),
            Some(&self.subscription_transition_lock),
            Some(&self.subscription_ownership),
            Some(self.fetching_accounts.as_ref()),
            Some(&self.capacity_eviction_protection),
        )
        .await
    }
}
