use std::{
    path::{Path, PathBuf},
    sync::{atomic::AtomicU64, Arc},
    time::Duration,
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
    _pubsub_client: ChainPubsubClientMock,
    _forward_rx: mpsc::Receiver<ForwardedSubscriptionUpdate>,
}

async fn setup_provider(
    pubkey: solana_pubkey::Pubkey,
    account: Account,
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
    let (subscribed_accounts, config) = create_test_lru_cache(1000);
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
        _pubsub_client: pubsub_client,
        _forward_rx: forward_rx,
    }
}

struct TestSlotConfig {
    current_slot: u64,
    account1_slot: u64,
    account2_slot: u64,
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
async fn test_try_get_multi_setup_subscriptions_failure_cleans_up_pending_entry(
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
        _pubsub_client,
        _forward_rx,
        ..
    } = setup_provider(pubkey, account).await;

    _pubsub_client.block_subscribe();

    let task_handle = tokio::spawn({
        let provider = provider.clone();
        async move {
            provider
                .try_get_multi(
                    &[pubkey],
                    None,
                    AccountFetchOrigin::GetAccount,
                    None,
                    None,
                )
                .await
        }
    });

    _pubsub_client.wait_for_subscribe_attempts(1).await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(provider.is_pending(&pubkey));

    _pubsub_client.simulate_disconnect();
    _pubsub_client.release_subscribe();

    let result = tokio::time::timeout(Duration::from_secs(2), task_handle)
        .await
        .expect("owner task should complete")
        .expect("owner task should not panic");
    let err = result.expect_err("setup_subscriptions should fail");
    assert!(err.to_string().contains("subscription(s) failed"));
    assert!(!provider.is_pending(&pubkey));

    _pubsub_client.try_reconnect().await.unwrap();
    let retry = provider
        .try_get_multi(
            &[pubkey],
            None,
            AccountFetchOrigin::GetAccount,
            None,
            None,
        )
        .await
        .expect("retry after cleanup should succeed");
    assert_eq!(retry.len(), 1);
}

#[tokio::test]
async fn test_try_get_multi_waiter_receives_setup_subscriptions_failure() {
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
        _pubsub_client,
        _forward_rx,
        ..
    } = setup_provider(pubkey, account).await;

    _pubsub_client.block_subscribe();

    let first_task_handle = tokio::spawn({
        let provider = provider.clone();
        async move {
            provider
                .try_get_multi(
                    &[pubkey],
                    None,
                    AccountFetchOrigin::GetAccount,
                    None,
                    None,
                )
                .await
        }
    });

    _pubsub_client.wait_for_subscribe_attempts(1).await;
    tokio::time::sleep(Duration::from_millis(50)).await;

    let second_task_handle = tokio::spawn({
        let provider = provider.clone();
        async move {
            provider
                .try_get_multi(
                    &[pubkey],
                    None,
                    AccountFetchOrigin::GetAccount,
                    None,
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

    _pubsub_client.simulate_disconnect();
    _pubsub_client.release_subscribe();

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
        _pubsub_client,
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
        .release_subscription(&pubkey, SubscriptionReason::DirectAccount)
        .await
        .unwrap();

    assert!(!unsubscribed);
    assert!(provider.is_watching(&pubkey));
    assert!(_pubsub_client.subscriptions_union().contains(&pubkey));

    let unsubscribed = provider
        .release_subscription(&pubkey, SubscriptionReason::DirectAccount)
        .await
        .unwrap();

    assert!(unsubscribed);
    assert!(!provider.is_watching(&pubkey));
    assert!(!_pubsub_client.subscriptions_union().contains(&pubkey));
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
        _pubsub_client,
        _forward_rx,
        ..
    } = setup_provider(pubkey, account).await;

    provider
        .acquire_subscription(&pubkey, SubscriptionReason::DirectAccount)
        .await
        .unwrap();

    let unsubscribed = provider
        .release_subscription(&pubkey, SubscriptionReason::DirectAccount)
        .await
        .unwrap();

    assert!(unsubscribed);
    assert!(!provider.is_watching(&pubkey));
    assert!(!_pubsub_client.subscriptions_union().contains(&pubkey));
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
        _pubsub_client,
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
        .release_subscription(&pubkey, SubscriptionReason::DirectAccount)
        .await
        .unwrap();

    assert!(!unsubscribed);
    assert!(provider.is_watching(&pubkey));
    assert!(_pubsub_client.subscriptions_union().contains(&pubkey));

    let unsubscribed = provider
        .release_subscription(&pubkey, SubscriptionReason::DelegationRecord)
        .await
        .unwrap();

    assert!(unsubscribed);
    assert!(!provider.is_watching(&pubkey));
    assert!(!_pubsub_client.subscriptions_union().contains(&pubkey));
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
        _pubsub_client,
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
        provider
            .release_subscription(&pubkey, SubscriptionReason::DirectAccount,)
    );
    acquire_result.unwrap();
    let unsubscribed = release_result.unwrap();

    assert!(!unsubscribed);
    assert!(provider.is_watching(&pubkey));
    assert!(_pubsub_client.subscriptions_union().contains(&pubkey));

    let unsubscribed = provider
        .release_subscription(&pubkey, SubscriptionReason::DelegationRecord)
        .await
        .unwrap();

    assert!(unsubscribed);
    assert!(!provider.is_watching(&pubkey));
    assert!(!_pubsub_client.subscriptions_union().contains(&pubkey));
}

#[tokio::test]
async fn test_try_get_multi_owner_success_cleans_up_pending_entry() {
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
                    AccountFetchOrigin::GetAccount,
                    None,
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
        .try_get(pubkey, AccountFetchOrigin::GetAccount)
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
        .try_get(pubkey, AccountFetchOrigin::GetAccount)
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
            }),
            AccountFetchOrigin::GetAccount,
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
            }),
            AccountFetchOrigin::GetAccount,
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
async fn test_get_accounts_until_slots_match_finding_matching_slot_but_chain_slot_smaller_than_min_context_slot(
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

    let res = remote_account_provider
        .try_get_multi_until_slots_match(
            &[pubkey1, pubkey2],
            Some(MatchSlotsConfig {
                max_retries: 10,
                retry_interval_ms: 50,
                min_context_slot: Some(CURRENT_SLOT + 1),
            }),
            AccountFetchOrigin::GetAccount,
        )
        .await;

    debug!(result = ?res, "Result");

    assert!(res.is_err());
    assert!(matches!(
        res.unwrap_err(),
        RemoteAccountProviderError::MatchingSlotsNotSatisfyingMinContextSlot(
            _pubkeys,
            _slots,
            slot,
            _
        ) if slot == CURRENT_SLOT + 1
    ));
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
            }),
            AccountFetchOrigin::GetAccount,
        )
        .await;

    debug!(result = ?res, "Result");

    assert!(res.is_ok());
    let accs = res.unwrap();

    assert_eq!(accs.len(), 2);
    assert!(accs[0].is_found());
    assert!(!accs[1].is_found());
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
            .try_get(*pk, AccountFetchOrigin::GetAccount)
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
        .try_get(pubkey1, AccountFetchOrigin::GetAccount)
        .await
        .unwrap();
    provider
        .try_get(pubkey2, AccountFetchOrigin::GetAccount)
        .await
        .unwrap();
    provider
        .try_get(pubkey3, AccountFetchOrigin::GetAccount)
        .await
        .unwrap();

    // Access pubkey1 to make it more recently used: [2, 3, 1]
    // This should just promote, making order [2, 3, 1]
    provider
        .try_get(pubkey1, AccountFetchOrigin::GetAccount)
        .await
        .unwrap();

    // Add pubkey4, should evict pubkey2 (now least recently used)
    provider
        .try_get(pubkey4, AccountFetchOrigin::GetAccount)
        .await
        .unwrap();

    // Check channel received the evicted account

    let removed_accounts = drain_removed_account_rx(&mut removed_rx);
    assert_eq!(removed_accounts, [pubkey2]);

    // Add pubkey5, should evict pubkey3 (now least recently used)
    provider
        .try_get(pubkey5, AccountFetchOrigin::GetAccount)
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
            .try_get(*pk, AccountFetchOrigin::GetAccount)
            .await
            .unwrap();
    }

    // Add more accounts and verify evictions happen in LRU order
    for i in 4..7 {
        provider
            .try_get(pubkeys[i], AccountFetchOrigin::GetAccount)
            .await
            .unwrap();
        let expected_evicted = pubkeys[i - 4]; // Should evict the account added 4 steps ago

        // Verify the evicted account was sent over the channel
        let removed_accounts = drain_removed_account_rx(&mut removed_rx);
        assert_eq!(removed_accounts, vec![expected_evicted]);
    }
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
