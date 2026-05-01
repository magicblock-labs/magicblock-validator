use std::{
    sync::{atomic::AtomicU64, Arc},
    time::Duration,
};

use solana_account::Account;
use tokio::sync::mpsc;

use super::*;
use crate::{
    remote_account_provider::{
        chain_pubsub_client::mock::ChainPubsubClientMock, chain_slot::ChainSlot,
    },
    testing::{
        rpc_client_mock::{ChainRpcClientMock, ChainRpcClientMockBuilder},
        utils::create_test_lru_cache,
    },
};

struct ProviderTestCtx {
    provider:
        Arc<RemoteAccountProvider<ChainRpcClientMock, ChainPubsubClientMock>>,
    _pubsub_client: ChainPubsubClientMock,
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

    let (forward_tx, _forward_rx) = mpsc::channel(1_000);
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
    }
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

    // Give the second task time to queue as a waiter
    tokio::time::sleep(Duration::from_millis(50)).await;

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
async fn test_stale_fetching_account_entry_is_evicted_and_replaced() {
    let pubkey = solana_pubkey::Pubkey::new_unique();
    let account = Account {
        lamports: 1_000_000,
        data: vec![1, 2, 3, 4],
        owner: solana_pubkey::Pubkey::new_unique(),
        executable: false,
        rent_epoch: 0,
    };

    let ProviderTestCtx { provider, .. } =
        setup_provider(pubkey, account.clone()).await;

    // Directly insert a stale FetchingAccountState into the map
    // Use 35 seconds (stale threshold is 30)
    let stale_created_at = std::time::Instant::now() - Duration::from_secs(35);
    {
        let mut fetching = provider.fetching_accounts.lock().unwrap();
        fetching.insert(
            pubkey,
            FetchingAccountState {
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
