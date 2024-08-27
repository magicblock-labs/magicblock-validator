use std::time::Duration;

use conjunto_transwise::RpcProviderConfig;
use sleipnir_account_updates::{
    AccountUpdates, RemoteAccountUpdatesClient, RemoteAccountUpdatesWorker,
    RemoteAccountUpdatesWorkerError,
};
use solana_sdk::{
    signature::Keypair,
    signer::Signer,
    system_program,
    sysvar::{clock, rent, slot_hashes},
};
use test_tools::skip_if_devnet_down;
use tokio::time::sleep;
use tokio_util::sync::CancellationToken;

fn setup() -> (
    RemoteAccountUpdatesClient,
    CancellationToken,
    tokio::task::JoinHandle<Result<(), RemoteAccountUpdatesWorkerError>>,
) {
    // Create account updates worker and client
    let mut worker =
        RemoteAccountUpdatesWorker::new(RpcProviderConfig::devnet());
    let client = RemoteAccountUpdatesClient::new(&worker);
    // Run the worker in a separate task
    let cancellation_token = CancellationToken::new();
    let worker_handle = {
        let cancellation_token = cancellation_token.clone();
        tokio::spawn(async move {
            worker
                .start_monitoring_request_listener(cancellation_token)
                .await
        })
    };
    // Ready to run
    (client, cancellation_token, worker_handle)
}

#[tokio::test]
async fn test_devnet_monitoring_clock_sysvar_changes() {
    skip_if_devnet_down!();
    // Create account updates worker and client
    let (client, cancellation_token, worker_handle) = setup();
    // The clock will change every slots, perfect for testing updates
    let sysvar_clock = clock::ID;
    // Before starting the monitoring, we should know nothing about the clock
    assert!(client.get_last_known_update_slot(&sysvar_clock).is_none());
    // Start the monitoring
    assert!(client.ensure_account_monitoring(&sysvar_clock).is_ok());
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
    skip_if_devnet_down!();
    // Create account updates worker and client
    let (client, cancellation_token, worker_handle) = setup();
    // Devnet accounts to be monitored for this test
    let sysvar_rent = rent::ID;
    let sysvar_sh = slot_hashes::ID;
    let sysvar_clock = clock::ID;
    // We shouldnt known anything about the accounts until we subscribe
    assert!(client.get_last_known_update_slot(&sysvar_rent).is_none());
    assert!(client.get_last_known_update_slot(&sysvar_sh).is_none());
    assert!(client.get_last_known_update_slot(&sysvar_clock).is_none());
    // Start monitoring the accounts now
    assert!(client.ensure_account_monitoring(&sysvar_rent).is_ok());
    assert!(client.ensure_account_monitoring(&sysvar_sh).is_ok());
    assert!(client.ensure_account_monitoring(&sysvar_clock).is_ok());
    // Wait for a few slots to happen on-chain
    sleep(Duration::from_millis(3000)).await;
    // Check that we detected the accounts changes
    assert!(client.get_last_known_update_slot(&sysvar_rent).is_none()); // Rent doesn't change
    assert!(client.get_last_known_update_slot(&sysvar_sh).is_some());
    assert!(client.get_last_known_update_slot(&sysvar_clock).is_some());
    // Cleanup everything correctly
    cancellation_token.cancel();
    assert!(worker_handle.await.is_ok());
}

#[tokio::test]
async fn test_devnet_monitoring_some_accounts_only() {
    skip_if_devnet_down!();
    // Create account updates worker and client
    let (client, cancellation_token, worker_handle) = setup();
    // Devnet accounts for this test
    let sysvar_rent = rent::ID;
    let sysvar_sh = slot_hashes::ID;
    let sysvar_clock = clock::ID;
    // We shouldnt known anything about the accounts until we subscribe
    assert!(client.get_last_known_update_slot(&sysvar_rent).is_none());
    assert!(client.get_last_known_update_slot(&sysvar_sh).is_none());
    assert!(client.get_last_known_update_slot(&sysvar_clock).is_none());
    // Start monitoring only some of the accounts
    assert!(client.ensure_account_monitoring(&sysvar_rent).is_ok());
    assert!(client.ensure_account_monitoring(&sysvar_sh).is_ok());
    // Wait for a few slots to happen on-chain
    sleep(Duration::from_millis(3000)).await;
    // Check that we detected the accounts changes only on the accounts we monitored
    assert!(client.get_last_known_update_slot(&sysvar_rent).is_none()); // Rent doesn't change
    assert!(client.get_last_known_update_slot(&sysvar_sh).is_some());
    assert!(client.get_last_known_update_slot(&sysvar_clock).is_none());
    // Cleanup everything correctly
    cancellation_token.cancel();
    assert!(worker_handle.await.is_ok());
}

#[tokio::test]
async fn test_devnet_monitoring_invalid_and_immutable_and_program_account() {
    skip_if_devnet_down!();
    // Create account updates worker and client
    let (client, cancellation_token, worker_handle) = setup();
    // Devnet accounts for this test (none of them should change)
    let new_account = Keypair::new().pubkey();
    let system_program = system_program::ID;
    let sysvar_rent = rent::ID;
    // We shouldnt known anything about the accounts until we subscribe
    assert!(client.get_last_known_update_slot(&new_account).is_none());
    assert!(client.get_last_known_update_slot(&system_program).is_none());
    assert!(client.get_last_known_update_slot(&sysvar_rent).is_none());
    // Start monitoring all accounts
    assert!(client.ensure_account_monitoring(&new_account).is_ok());
    assert!(client.ensure_account_monitoring(&system_program).is_ok());
    assert!(client.ensure_account_monitoring(&sysvar_rent).is_ok());
    // Wait for a few slots to happen on-chain
    sleep(Duration::from_millis(3000)).await;
    // We shouldnt have detected any change whatsoever on those
    assert!(client.get_last_known_update_slot(&new_account).is_none());
    assert!(client.get_last_known_update_slot(&system_program).is_none());
    assert!(client.get_last_known_update_slot(&sysvar_rent).is_none());
    // Cleanup everything correctly (nothing should have failed tho)
    cancellation_token.cancel();
    assert!(worker_handle.await.is_ok());
}
