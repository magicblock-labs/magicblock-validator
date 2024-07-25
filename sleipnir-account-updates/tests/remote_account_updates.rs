use std::{str::FromStr, time::Duration};

use conjunto_transwise::RpcProviderConfig;
use sleipnir_account_updates::{
    AccountUpdates, RemoteAccountUpdates, RemoteAccountUpdatesWatcher,
};
use solana_sdk::pubkey::Pubkey;
use tokio::time::sleep;
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn test_devnet_monitoring_clock_sysvar_changes() {
    // Create account updates watcher
    let mut watcher =
        RemoteAccountUpdatesWatcher::new(RpcProviderConfig::devnet());
    let reader = RemoteAccountUpdates::new(&watcher);
    // Run the watcher in a separate task
    let cancellation_token = CancellationToken::new();
    let watcher_handle = {
        let cancellation_token = cancellation_token.clone();
        tokio::spawn(async move { watcher.run(cancellation_token).await })
    };
    // Start monitoring the clock
    let sysvar_clock =
        Pubkey::from_str("SysvarC1ock11111111111111111111111111111111")
            .unwrap();
    assert!(!reader.has_known_update_since_slot(&sysvar_clock, 0));
    reader.request_account_monitoring(&sysvar_clock);
    // Wait a few slots to pass
    sleep(Duration::from_millis(3000)).await;
    // Check that we detected the clock change
    assert!(reader.has_known_update_since_slot(&sysvar_clock, 0));
    // Cleanup everything correctly
    cancellation_token.cancel();
    assert!(watcher_handle.await.is_ok());
}

#[tokio::test]
async fn test_devnet_monitoring_multiple_accounts_at_the_same_time() {
    // Create account updates watcher
    let mut watcher =
        RemoteAccountUpdatesWatcher::new(RpcProviderConfig::devnet());
    let reader = RemoteAccountUpdates::new(&watcher);
    // Run the watcher in a separate task
    let cancellation_token = CancellationToken::new();
    let watcher_handle = {
        let cancellation_token = cancellation_token.clone();
        tokio::spawn(async move { watcher.run(cancellation_token).await })
    };
    // Devnet accounts to be monitored for this test
    let sysvar_blockhashes =
        Pubkey::from_str("SysvarRecentB1ockHashes11111111111111111111")
            .unwrap();
    let sysvar_clock =
        Pubkey::from_str("SysvarC1ock11111111111111111111111111111111")
            .unwrap();
    // We shouldnt known anything about the accounts until we subscribe
    assert!(!reader.has_known_update_since_slot(&sysvar_blockhashes, 0));
    assert!(!reader.has_known_update_since_slot(&sysvar_clock, 0));
    // Start monitoring the accounts now
    reader.request_account_monitoring(&sysvar_blockhashes);
    reader.request_account_monitoring(&sysvar_clock);
    // Wait a few slots to pass
    sleep(Duration::from_millis(3000)).await;
    // Check that we detected the accounts changes
    assert!(reader.has_known_update_since_slot(&sysvar_blockhashes, 0));
    assert!(reader.has_known_update_since_slot(&sysvar_clock, 0));
    // Cleanup everything correctly
    cancellation_token.cancel();
    assert!(watcher_handle.await.is_ok());
}

#[tokio::test]
async fn test_devnet_monitoring_some_accounts_only() {
    // Create account updates watcher
    let mut watcher =
        RemoteAccountUpdatesWatcher::new(RpcProviderConfig::devnet());
    let reader = RemoteAccountUpdates::new(&watcher);
    // Run the watcher in a separate task
    let cancellation_token = CancellationToken::new();
    let watcher_handle = {
        let cancellation_token = cancellation_token.clone();
        tokio::spawn(async move { watcher.run(cancellation_token).await })
    };
    // Devnet accounts for this test
    let sysvar_blockhashes =
        Pubkey::from_str("SysvarRecentB1ockHashes11111111111111111111")
            .unwrap();
    let sysvar_clock =
        Pubkey::from_str("SysvarC1ock11111111111111111111111111111111")
            .unwrap();
    // We shouldnt known anything about the accounts until we subscribe
    assert!(!reader.has_known_update_since_slot(&sysvar_blockhashes, 0));
    assert!(!reader.has_known_update_since_slot(&sysvar_clock, 0));
    // Start monitoring only some of the accounts
    reader.request_account_monitoring(&sysvar_blockhashes);
    // Wait a few slots to pass
    sleep(Duration::from_millis(3000)).await;
    // Check that we detected the accounts changes only on the accounts we monitored
    assert!(reader.has_known_update_since_slot(&sysvar_blockhashes, 0));
    assert!(!reader.has_known_update_since_slot(&sysvar_clock, 0));
    // Cleanup everything correctly
    cancellation_token.cancel();
    assert!(watcher_handle.await.is_ok());
}

#[tokio::test]
async fn test_devnet_monitoring_invalid_and_immutable_and_program_account() {
    // Create account updates watcher
    let mut watcher =
        RemoteAccountUpdatesWatcher::new(RpcProviderConfig::devnet());
    let reader = RemoteAccountUpdates::new(&watcher);
    // Run the watcher in a separate task
    let cancellation_token = CancellationToken::new();
    let watcher_handle = {
        let cancellation_token = cancellation_token.clone();
        tokio::spawn(async move { watcher.run(cancellation_token).await })
    };
    // Devnet accounts for this test
    let unknown_account = Pubkey::new_unique();
    let system_program =
        Pubkey::from_str("11111111111111111111111111111111").unwrap();
    let sysvar_rent =
        Pubkey::from_str("SysvarRent111111111111111111111111111111111")
            .unwrap();
    // We shouldnt known anything about the accounts until we subscribe
    assert!(!reader.has_known_update_since_slot(&unknown_account, 0));
    assert!(!reader.has_known_update_since_slot(&system_program, 0));
    assert!(!reader.has_known_update_since_slot(&sysvar_rent, 0));
    // Start monitoring all accounts
    reader.request_account_monitoring(&unknown_account);
    reader.request_account_monitoring(&system_program);
    reader.request_account_monitoring(&sysvar_rent);
    // Wait a few slots to pass
    sleep(Duration::from_millis(3000)).await;
    // We shouldnt have detected any change whatsoever on those
    assert!(!reader.has_known_update_since_slot(&unknown_account, 0));
    assert!(!reader.has_known_update_since_slot(&system_program, 0));
    assert!(!reader.has_known_update_since_slot(&sysvar_rent, 0));
    // Cleanup everything correctly (nothing should have failed tho)
    cancellation_token.cancel();
    assert!(watcher_handle.await.is_ok());
}
