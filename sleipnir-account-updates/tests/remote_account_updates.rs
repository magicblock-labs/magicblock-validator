use std::time::Duration;

use conjunto_transwise::RpcProviderConfig;
use sleipnir_account_updates::{
    AccountUpdates, RemoteAccountUpdatesClient, RemoteAccountUpdatesWorker,
};
use solana_sdk::{
    pubkey::Pubkey,
    signature::Keypair,
    signer::Signer,
    sysvar::{clock, recent_blockhashes, rent},
};
use tokio::time::sleep;
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn test_devnet_monitoring_clock_sysvar_changes() {
    // Create account updates worker and client
    let mut worker =
        RemoteAccountUpdatesWorker::new(RpcProviderConfig::devnet());
    let client = RemoteAccountUpdatesClient::new(&worker);
    // Run the worker in a separate task
    let cancellation_token = CancellationToken::new();
    let worker_handle = {
        let cancellation_token = cancellation_token.clone();
        tokio::spawn(async move {
            worker.start_monitoring(cancellation_token).await
        })
    };
    // Start monitoring the clock
    let sysvar_clock = clock::ID;
    assert!(client.get_last_known_update_slot(&sysvar_clock).is_none());
    client.request_start_account_monitoring(&sysvar_clock);
    // Wait for a few slots to happen on-chain
    sleep(Duration::from_millis(3000)).await;
    // Check that we detected the clock change
    assert!(client.get_last_known_update_slot(&sysvar_clock).is_some());
    // Cleanup everything correctly
    cancellation_token.cancel();
    assert!(worker_handle.await.is_ok());
}

#[tokio::test]
async fn test_devnet_monitoring_multiple_accounts_at_the_same_time() {
    // Create account updates worker and client
    let mut worker =
        RemoteAccountUpdatesWorker::new(RpcProviderConfig::devnet());
    let client = RemoteAccountUpdatesClient::new(&worker);
    // Run the worker in a separate task
    let cancellation_token = CancellationToken::new();
    let worker_handle = {
        let cancellation_token = cancellation_token.clone();
        tokio::spawn(async move {
            worker.start_monitoring(cancellation_token).await
        })
    };
    // Devnet accounts to be monitored for this test
    let sysvar_blockhashes = recent_blockhashes::ID;
    let sysvar_clock = clock::ID;
    // We shouldnt known anything about the accounts until we subscribe
    assert!(client
        .get_last_known_update_slot(&sysvar_blockhashes)
        .is_none());
    assert!(client.get_last_known_update_slot(&sysvar_clock).is_none());
    // Start monitoring the accounts now
    client.request_start_account_monitoring(&sysvar_blockhashes);
    client.request_start_account_monitoring(&sysvar_clock);
    // Wait for a few slots to happen on-chain
    sleep(Duration::from_millis(3000)).await;
    // Check that we detected the accounts changes
    assert!(client
        .get_last_known_update_slot(&sysvar_blockhashes)
        .is_some());
    assert!(client.get_last_known_update_slot(&sysvar_clock).is_some());
    // Cleanup everything correctly
    cancellation_token.cancel();
    assert!(worker_handle.await.is_ok());
}

#[tokio::test]
async fn test_devnet_monitoring_some_accounts_only() {
    // Create account updates worker and client
    let mut worker =
        RemoteAccountUpdatesWorker::new(RpcProviderConfig::devnet());
    let client = RemoteAccountUpdatesClient::new(&worker);
    // Run the worker in a separate task
    let cancellation_token = CancellationToken::new();
    let worker_handle = {
        let cancellation_token = cancellation_token.clone();
        tokio::spawn(async move {
            worker.start_monitoring(cancellation_token).await
        })
    };
    // Devnet accounts for this test
    let sysvar_blockhashes = recent_blockhashes::ID;
    let sysvar_clock = solana_sdk::sysvar::clock::ID;
    // We shouldnt known anything about the accounts until we subscribe
    assert!(client
        .get_last_known_update_slot(&sysvar_blockhashes)
        .is_none());
    assert!(client.get_last_known_update_slot(&sysvar_clock).is_none());
    // Start monitoring only some of the accounts
    client.request_start_account_monitoring(&sysvar_blockhashes);
    // Wait for a few slots to happen on-chain
    sleep(Duration::from_millis(3000)).await;
    // Check that we detected the accounts changes only on the accounts we monitored
    assert!(client
        .get_last_known_update_slot(&sysvar_blockhashes)
        .is_some());
    assert!(client.get_last_known_update_slot(&sysvar_clock).is_none());
    // Cleanup everything correctly
    cancellation_token.cancel();
    assert!(worker_handle.await.is_ok());
}

#[tokio::test]
async fn test_devnet_monitoring_invalid_and_immutable_and_program_account() {
    // Create account updates worker and client
    let mut worker =
        RemoteAccountUpdatesWorker::new(RpcProviderConfig::devnet());
    let client = RemoteAccountUpdatesClient::new(&worker);
    // Run the worker in a separate task
    let cancellation_token = CancellationToken::new();
    let worker_handle = {
        let cancellation_token = cancellation_token.clone();
        tokio::spawn(async move {
            worker.start_monitoring(cancellation_token).await
        })
    };
    // Devnet accounts for this test
    let unknown_account = Keypair::new().pubkey();
    let system_program = solana_sdk::system_program::ID;
    let sysvar_rent = rent::ID;
    // We shouldnt known anything about the accounts until we subscribe
    assert!(client
        .get_last_known_update_slot(&unknown_account)
        .is_none());
    assert!(client.get_last_known_update_slot(&system_program).is_none());
    assert!(client.get_last_known_update_slot(&sysvar_rent).is_none());
    // Start monitoring all accounts
    client.request_start_account_monitoring(&unknown_account);
    client.request_start_account_monitoring(&system_program);
    client.request_start_account_monitoring(&sysvar_rent);
    // Wait for a few slots to happen on-chain
    sleep(Duration::from_millis(3000)).await;
    // We shouldnt have detected any change whatsoever on those
    assert!(client
        .get_last_known_update_slot(&unknown_account)
        .is_none());
    assert!(client.get_last_known_update_slot(&system_program).is_none());
    assert!(client.get_last_known_update_slot(&sysvar_rent).is_none());
    // Cleanup everything correctly (nothing should have failed tho)
    cancellation_token.cancel();
    assert!(worker_handle.await.is_ok());
}
