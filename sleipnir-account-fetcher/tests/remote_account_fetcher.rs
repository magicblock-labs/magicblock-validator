use std::time::Duration;

use conjunto_transwise::RpcProviderConfig;
use sleipnir_account_fetcher::{
    AccountFetcher, RemoteAccountFetcherClient, RemoteAccountFetcherWorker,
};
use solana_sdk::{
    signature::Keypair,
    signer::Signer,
    system_program,
    sysvar::{clock, recent_blockhashes, rent},
};
use tokio::time::sleep;
use tokio_util::sync::CancellationToken;

fn setup() -> (
    RemoteAccountFetcherClient,
    CancellationToken,
    tokio::task::JoinHandle<()>,
) {
    // Create account fetcher worker and client
    let mut worker =
        RemoteAccountFetcherWorker::new(RpcProviderConfig::devnet());
    let client = RemoteAccountFetcherClient::new(&worker);
    // Run the worker in a separate task
    let cancellation_token = CancellationToken::new();
    let worker_handle = {
        let cancellation_token = cancellation_token.clone();
        tokio::spawn(
            async move { worker.start_fetchings(cancellation_token).await },
        )
    };
    // Ready to run
    (client, cancellation_token, worker_handle)
}

#[tokio::test]
async fn test_devnet_fetch_clock_multiple_times() {
    // Create account fetcher worker and client
    let (client, cancellation_token, worker_handle) = setup();
    // Sysvar clock should change every slot
    let key_sysvar_clock = clock::ID;
    // Start to fetch the clock now
    let future_clock1 = client.fetch_account_chain_snapshot(&key_sysvar_clock);
    // Start to fetch the clock immediately again, we should not have any reply yet from the first one
    let future_clock2 = client.fetch_account_chain_snapshot(&key_sysvar_clock);
    // Wait for a few slots to happen on-chain
    sleep(Duration::from_millis(3000)).await;
    // Start to fetch the clock again, it should have changed on chain (and the first fetch should have finished)
    let future_clock3 = client.fetch_account_chain_snapshot(&key_sysvar_clock);
    // Await all results to be available
    let result_clock1 = future_clock1.await;
    let result_clock2 = future_clock2.await;
    let result_clock3 = future_clock3.await;
    // All should have succeeded
    assert!(result_clock1.is_ok());
    assert!(result_clock2.is_ok());
    assert!(result_clock3.is_ok());
    // The first 2 fetch should get the same result, but the 3rd one should get a different clock
    let snapshot_clock1 = result_clock1.unwrap();
    let snapshot_clock2 = result_clock2.unwrap();
    let snapshot_clock3 = result_clock3.unwrap();
    assert_eq!(snapshot_clock1, snapshot_clock2);
    assert_ne!(snapshot_clock1, snapshot_clock3);
    // Cleanup everything correctly
    cancellation_token.cancel();
    assert!(worker_handle.await.is_ok());
}

#[tokio::test]
async fn test_devnet_fetch_multiple_accounts_same_time() {
    // Create account fetcher worker and client
    let (client, cancellation_token, worker_handle) = setup();
    // A few accounts we'd want to try to fetch at the same time
    let key_system_program = system_program::ID;
    let key_sysvar_blockhashes = recent_blockhashes::ID;
    let key_sysvar_clock = clock::ID;
    let key_sysvar_rent = rent::ID;
    let key_new_account = Keypair::new().pubkey();
    // Fetch all of them at the same time
    let future_system_program =
        client.fetch_account_chain_snapshot(&key_system_program);
    let future_sysvar_blockhashes =
        client.fetch_account_chain_snapshot(&key_sysvar_blockhashes);
    let future_sysvar_clock =
        client.fetch_account_chain_snapshot(&key_sysvar_clock);
    let future_sysvar_rent =
        client.fetch_account_chain_snapshot(&key_sysvar_rent);
    let future_new_account =
        client.fetch_account_chain_snapshot(&key_new_account);
    // Await all results
    let result_system_program = future_system_program.await;
    let result_sysvar_blockhashes = future_sysvar_blockhashes.await;
    let result_sysvar_clock = future_sysvar_clock.await;
    let result_sysvar_rent = future_sysvar_rent.await;
    let result_new_account = future_new_account.await;
    // Check that there ws no error
    assert!(result_system_program.is_ok());
    assert!(result_sysvar_blockhashes.is_ok());
    assert!(result_sysvar_clock.is_ok());
    assert!(result_sysvar_rent.is_ok());
    assert!(result_new_account.is_ok());
    // Unwraps
    let snapshot_system_program = result_system_program.unwrap();
    let snapshot_sysvar_blockhashes = result_sysvar_blockhashes.unwrap();
    let snapshot_sysvar_clock = result_sysvar_clock.unwrap();
    let snapshot_sysvar_rent = result_sysvar_rent.unwrap();
    let snapshot_new_account = result_new_account.unwrap();
    // Check addresses are matching
    assert_eq!(snapshot_system_program.pubkey, key_system_program);
    assert_eq!(snapshot_sysvar_blockhashes.pubkey, key_sysvar_blockhashes);
    assert_eq!(snapshot_sysvar_clock.pubkey, key_sysvar_clock);
    assert_eq!(snapshot_sysvar_rent.pubkey, key_sysvar_rent);
    assert_eq!(snapshot_new_account.pubkey, key_new_account);
    // Extra checks
    assert!(snapshot_system_program.chain_state.is_undelegated());
    assert!(snapshot_sysvar_blockhashes.chain_state.is_undelegated());
    assert!(snapshot_sysvar_clock.chain_state.is_undelegated());
    assert!(snapshot_sysvar_rent.chain_state.is_undelegated());
    assert!(snapshot_new_account.chain_state.is_new());
    // Cleanup everything correctly
    cancellation_token.cancel();
    assert!(worker_handle.await.is_ok());
}
