use std::{
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
use tokio::sync::mpsc;

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
    assert!(err.to_string().contains("subscription(s) failed"));
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
    assert!(first_err.to_string().contains("subscription(s) failed"));
    assert!(second_err.to_string().contains("subscription(s) failed"));
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

    tokio::time::sleep(Duration::from_millis(50)).await;

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
    assert!(!pubsub_client.subscriptions_union().contains(&pubkey1));
    assert!(pubsub_client.subscriptions_union().contains(&pubkey2));
    assert!(!provider
        .subscription_ownership
        .lock()
        .await
        .contains_key(&pubkey1));
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
    assert!(!pubsub_client.subscriptions_union().contains(&pubkey1));
    assert!(pubsub_client.subscriptions_union().contains(&pubkey2));
    assert!(!provider
        .subscription_ownership
        .lock()
        .await
        .contains_key(&pubkey1));
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
        .account(pubkey, account)
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

    rpc_client.allow_fetches();
    wait_for_pending_account_delta_at_least(
        fetch_context,
        ChainlinkPendingFetchOutcome::RpcFetchCompletedAfterUpdate,
        late_rpc_baseline,
        1,
    )
    .await;
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
                companion_fetch_kind:
                    ChainlinkCompanionFetchKind::GenericSlotMatch,
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
                            ChainlinkCompanionFetchKind::GenericSlotMatch,
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
                companion_fetch_kind:
                    ChainlinkCompanionFetchKind::GenericSlotMatch,
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
                companion_fetch_kind:
                    ChainlinkCompanionFetchKind::GenericSlotMatch,
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
                companion_fetch_kind:
                    ChainlinkCompanionFetchKind::GenericSlotMatch,
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
    let config = MatchSlotsConfig {
        max_retries: 10,
        retry_interval_ms: 50,
        min_context_slot: None,
        companion_fetch_kind: ChainlinkCompanionFetchKind::GenericSlotMatch,
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

    let removed_accounts = drain_removed_account_rx(&mut removed_rx);
    assert_eq!(removed_accounts, [pubkey2]);

    // Add pubkey5, should evict pubkey3 (now least recently used)
    provider
        .try_get(pubkey5, AccountFetchContext::rpc_get_account())
        .await
        .unwrap();

    // Check channel received the second evicted account
    let removed_accounts = drain_removed_account_rx(&mut removed_rx);
    assert_eq!(removed_accounts, [pubkey3]);
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
        let removed_accounts = drain_removed_account_rx(&mut removed_rx);
        assert_eq!(removed_accounts, vec![expected_evicted]);
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
    assert!(!provider
        .pubsub_client()
        .subscriptions_union()
        .contains(&pubkey1));
    assert!(provider
        .pubsub_client()
        .subscriptions_union()
        .contains(&pubkey2));

    let removed_accounts = drain_removed_account_rx(&mut removed_rx);
    assert_eq!(removed_accounts, [pubkey1]);
}

#[tokio::test]
async fn test_capacity_eviction_unsubscribe_failure_records_new_owner() {
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
    provider.pubsub_client().fail_next_unsubscriptions(1);

    let registration_before = registration_metric_value(
        SubscriptionRegistrationOrigin::Internal,
        SubscriptionReasonLabel::DirectAccount,
        SubscriptionRegistrationOutcome::UnsubscribeEvictedError,
    );
    let cleanup_before = cleanup_metric_value(
        SubscriptionCleanupSource::CapacityEviction,
        SubscriptionCleanupOutcome::UnsubscribeFailed,
    );

    let err = provider
        .acquire_subscription(&pubkey2, SubscriptionReason::DirectAccount)
        .await
        .unwrap_err();

    let registration_after = registration_metric_value(
        SubscriptionRegistrationOrigin::Internal,
        SubscriptionReasonLabel::DirectAccount,
        SubscriptionRegistrationOutcome::UnsubscribeEvictedError,
    );
    let cleanup_after = cleanup_metric_value(
        SubscriptionCleanupSource::CapacityEviction,
        SubscriptionCleanupOutcome::UnsubscribeFailed,
    );
    assert_eq!(registration_after - registration_before, 1);
    assert_eq!(cleanup_after - cleanup_before, 1);

    assert!(matches!(
        err,
        RemoteAccountProviderError::AccountSubscriptionsTaskFailed(_)
    ));
    assert!(provider.is_watching(&pubkey2));
    assert!(
        provider
            .has_subscription_reason(
                &pubkey2,
                SubscriptionReason::DirectAccount
            )
            .await
    );
    assert!(provider
        .pubsub_client()
        .subscriptions_union()
        .contains(&pubkey2));

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
    let cleanup_after = cleanup_metric_value(
        SubscriptionCleanupSource::CapacityEviction,
        SubscriptionCleanupOutcome::AlreadyAbsent,
    );
    assert_eq!(evicted_after - evicted_before, 1);
    assert_eq!(error_after - error_before, 0);
    assert_eq!(cleanup_after - cleanup_before, 1);

    assert!(!provider.is_watching(&pubkey1));
    assert!(provider.is_watching(&pubkey2));
    assert!(!provider
        .pubsub_client()
        .subscriptions_union()
        .contains(&pubkey1));
    assert!(provider
        .pubsub_client()
        .subscriptions_union()
        .contains(&pubkey2));
    assert!(!provider
        .subscription_ownership
        .lock()
        .await
        .contains_key(&pubkey1));

    let removed_accounts = drain_removed_account_rx(&mut removed_rx);
    assert_eq!(removed_accounts, [pubkey1]);
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
        SubscriptionRegistrationOutcome::RejectedAndUnsubscribed,
    );
    let cleanup_before = cleanup_metric_value(
        SubscriptionCleanupSource::RejectedNewSubscription,
        SubscriptionCleanupOutcome::Unsubscribed,
    );

    let err = provider
        .acquire_subscription(&pubkey3, SubscriptionReason::DirectAccount)
        .await
        .unwrap_err();

    let registration_after = registration_metric_value(
        SubscriptionRegistrationOrigin::Internal,
        SubscriptionReasonLabel::DirectAccount,
        SubscriptionRegistrationOutcome::RejectedAndUnsubscribed,
    );
    let cleanup_after = cleanup_metric_value(
        SubscriptionCleanupSource::RejectedNewSubscription,
        SubscriptionCleanupOutcome::Unsubscribed,
    );
    assert_eq!(registration_after - registration_before, 1);
    assert_eq!(cleanup_after - cleanup_before, 1);

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
    async fn reconcile_subscriptions_once_for_test(&self) -> usize {
        let never_evicted =
            self.lrucache_subscribed_accounts.never_evicted_accounts();
        subscription_reconciler::reconcile_subscriptions(
            &self.lrucache_subscribed_accounts,
            &self.pubsub_client,
            &never_evicted,
            &self.removed_account_tx,
            Some(&self.subscription_key_locks),
            Some(&self.subscription_ownership),
        )
        .await
    }
}
