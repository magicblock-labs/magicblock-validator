use std::time::Duration;

use conjunto_transwise::RpcProviderConfig;
use sleipnir_account_cloner::{
    AccountCloner, RemoteAccountClonerClient, RemoteAccountClonerWorker,
};
use sleipnir_account_fetcher::{
    AccountFetcherStub, RemoteAccountFetcherClient, RemoteAccountFetcherWorker,
};
use sleipnir_account_updates::{
    AccountUpdatesStub, RemoteAccountUpdatesClient, RemoteAccountUpdatesWorker,
};
use solana_sdk::sysvar::clock;
use test_tools::skip_if_devnet_down;
use tokio::time::sleep;
use tokio_util::sync::CancellationToken;

fn setup(
    account_fetcher: AccountFetcherStub,
    account_updates: AccountUpdatesStub,
) -> (
    RemoteAccountClonerClient,
    CancellationToken,
    tokio::task::JoinHandle<()>,
) {
    // Create account cloner worker and client
    let mut cloner_worker =
        RemoteAccountClonerWorker::new(account_fetcher, account_updates);
    let cloner_client = RemoteAccountClonerClient::new(&cloner_worker);
    // Run the worker in a separate task
    let cloner_worker_handle = {
        let cloner_cancellation_token = cancellation_token.clone();
        tokio::spawn(async move {
            cloner_worker
                .start_clone_request_processing(cloner_cancellation_token)
                .await
        })
    };
    // Ready to run
    (
        cloner_client,
        cancellation_token,
        tokio::spawn(async move {
            assert!(fetcher_worker_handle.await.is_ok());
            assert!(updates_worker_handle.await.is_ok());
            assert!(cloner_worker_handle.await.is_ok());
        }),
    )
}

#[tokio::test]
async fn test_devnet_clone_invalid_and_valid() {
    skip_if_devnet_down!();
    // Create account cloner worker and client
    let (client, cancellation_token, worker_handle) = setup();
    // Cloning the clock should be a no-op
    let key_sysvar_clock = clock::ID;
    let result_sysvar_clock = client.clone_account(&key_sysvar_clock).await;
    assert!(result_sysvar_clock.is_ok());

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
