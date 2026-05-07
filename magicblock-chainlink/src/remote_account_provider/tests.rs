use std::{
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
            rpc_client,
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
async fn test_try_get_multi_abort_during_setup_subscriptions_rolls_back_pending_entry(
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
    } = setup_provider(pubkey, account).await;

    // Block subscribe to hang the setup_subscriptions() call
    _pubsub_client.block_subscribe();

    // Spawn the first try_get_multi call which will block in setup_subscriptions
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

    // Wait for at least one subscribe attempt to ensure we're blocked in
    // setup_subscriptions
    _pubsub_client.wait_for_subscribe_attempts(1).await;

    // Give a small delay to ensure the entry is inserted in fetching_accounts
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Verify that the entry is pending
    assert!(provider.is_pending(&pubkey));

    // Abort the task which should trigger FetchingAccountGuard::Drop
    task_handle.abort();

    // Poll for at most 1 second until the pending entry is cleaned up by Drop
    let start = tokio::time::Instant::now();
    loop {
        if !provider.is_pending(&pubkey) {
            break;
        }
        if start.elapsed() > Duration::from_secs(1) {
            panic!("pending entry was not cleaned up within 1 second");
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    // Release the subscribe block
    _pubsub_client.release_subscribe();

    // Issue a second try_get_multi for the same pubkey
    // This should succeed since the first one's stale entry is gone
    let result = tokio::time::timeout(
        Duration::from_secs(2),
        provider.try_get_multi(
            &[pubkey],
            None,
            AccountFetchOrigin::GetAccount,
            None,
            None,
        ),
    )
    .await;

    assert!(
        result.is_ok(),
        "second try_get_multi should complete without hanging"
    );
    let result = result.unwrap();
    assert!(
        result.is_ok(),
        "second try_get_multi should succeed, got: {:?}",
        result
    );
    assert_eq!(result.unwrap().len(), 1);
}

#[tokio::test]
async fn test_try_get_multi_waiter_receives_error_when_owner_aborts_in_setup_subscriptions(
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
    } = setup_provider(pubkey, account).await;

    // Block subscribe to hang the setup_subscriptions() call
    _pubsub_client.block_subscribe();

    // Spawn the first try_get_multi call (the "owner")
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

    // Wait for the first subscribe attempt
    _pubsub_client.wait_for_subscribe_attempts(1).await;

    // Give a small delay to ensure the first task is blocked in setup_subscriptions
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Spawn a second try_get_multi call for the same pubkey (a "waiter")
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

    // Poll provider.fetching_accounts until the second task has registered
    // as a waiter on the entry keyed by pubkey before aborting the owner;
    // this avoids the race where first_task_handle.abort() runs before
    // second_task_handle has had a chance to attach itself to the entry's
    // waiters list.
    let waiter_registration_start = tokio::time::Instant::now();
    let waiter_registration_timeout = Duration::from_secs(2);
    loop {
        let waiter_count = {
            let fetching = provider.fetching_accounts.lock().unwrap();
            fetching.get(&pubkey).map(|s| s.waiters.len()).unwrap_or(0)
        };
        if waiter_count > 0 {
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

    // Abort the first (owner) task which should trigger FetchingAccountGuard::Drop
    // and fail all waiters with an explicit error
    first_task_handle.abort();

    // Release the subscribe block so any further calls can proceed
    _pubsub_client.release_subscribe();

    // Await the second task under timeout
    let result =
        tokio::time::timeout(Duration::from_secs(2), second_task_handle).await;

    assert!(
        result.is_ok(),
        "second task should complete without hanging"
    );
    let task_result = result.unwrap();
    assert!(
        task_result.is_ok(),
        "task should not panic, got: {:?}",
        task_result
    );
    let fetch_result = task_result.unwrap();
    assert!(
        fetch_result.is_err(),
        "second try_get_multi should return Err since owner was aborted, \
         got: {:?}",
        fetch_result
    );
    let err = fetch_result.unwrap_err();
    assert!(
        err.to_string().contains(
            "owner future dropped before setup_subscriptions completed"
        ),
        "error message should indicate owner cancellation, got: {}",
        err
    );
}

#[tokio::test]
async fn test_try_unsubscribe_if_sole_owner_preserves_created_subscription_ownership(
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
    } = setup_provider(pubkey, account).await;

    let rollback_token = match provider.subscribe(&pubkey).await.unwrap() {
        SubscribeResult::Created(token) => token,
        other => panic!("expected Created, got {other:?}"),
    };
    assert_eq!(
        provider.subscribe(&pubkey).await.unwrap(),
        SubscribeResult::AlreadyWatching
    );

    let unsubscribed = provider
        .try_unsubscribe_if_sole_owner(&pubkey, rollback_token)
        .await
        .unwrap();

    assert!(!unsubscribed);
    assert!(provider.is_watching(&pubkey));
    assert!(_pubsub_client.subscriptions_union().contains(&pubkey));
}

#[tokio::test]
async fn test_try_unsubscribe_if_sole_owner_removes_created_subscription() {
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
    } = setup_provider(pubkey, account).await;

    let rollback_token = match provider.subscribe(&pubkey).await.unwrap() {
        SubscribeResult::Created(token) => token,
        other => panic!("expected Created, got {other:?}"),
    };

    let unsubscribed = provider
        .try_unsubscribe_if_sole_owner(&pubkey, rollback_token)
        .await
        .unwrap();

    assert!(unsubscribed);
    assert!(!provider.is_watching(&pubkey));
    assert!(!_pubsub_client.subscriptions_union().contains(&pubkey));
}

#[tokio::test]
async fn test_stale_fetching_account_entry_is_evicted_and_replaced() {
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
        _forward_rx,
        ..
    } = setup_provider(pubkey, account.clone()).await;

    // Directly insert a stale FetchingAccountState into the map
    // Use 35 seconds (stale threshold is 30)
    let stale_created_at = std::time::Instant::now() - Duration::from_secs(35);
    {
        let mut fetching = provider.fetching_accounts.lock().unwrap();
        fetching.insert(
            pubkey,
            FetchingAccountState {
                generation: 1,
                created_at: stale_created_at,
                fetch_start_slot: 100,
                waiters: vec![],
            },
        );
    }

    // Verify stale entry is in the map
    assert!(provider.is_pending(&pubkey));

    // Call try_get_multi which should evict the stale entry
    let result = tokio::time::timeout(
        Duration::from_secs(2),
        provider.try_get_multi(
            &[pubkey],
            None,
            AccountFetchOrigin::GetAccount,
            None,
            None,
        ),
    )
    .await;

    assert!(
        result.is_ok(),
        "try_get_multi should complete without hanging"
    );
    let result = result.unwrap();
    assert!(
        result.is_ok(),
        "try_get_multi should succeed, got: {:?}",
        result
    );

    // Verify stale entry is gone
    assert!(!provider.is_pending(&pubkey));
}

#[test]
fn test_fetching_account_guard_drop_does_not_remove_replacement_entry() {
    let pubkey = solana_pubkey::Pubkey::new_unique();
    let fetching_accounts = Arc::<FetchingAccounts>::default();

    let old_generation = 1;
    let new_generation = 2;
    let guard = FetchingAccountGuard::new(
        fetching_accounts.clone(),
        pubkey,
        old_generation,
    );

    {
        let mut fetching = fetching_accounts.lock().unwrap();
        fetching.insert(
            pubkey,
            FetchingAccountState {
                generation: new_generation,
                created_at: std::time::Instant::now(),
                fetch_start_slot: 200,
                waiters: vec![],
            },
        );
    }

    drop(guard);

    let fetching = fetching_accounts.lock().unwrap();
    let state = fetching
        .get(&pubkey)
        .expect("replacement entry should remain after stale guard drop");
    assert_eq!(state.generation, new_generation);
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
