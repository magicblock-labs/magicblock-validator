use std::time::Duration;

use conjunto_transwise::RpcProviderConfig;
use sleipnir_account_updates::{
    AccountUpdates, RemoteAccountUpdatesReader, RemoteAccountUpdatesWatcher,
};
use solana_sdk::{
    pubkey::Pubkey,
    sysvar::{clock, recent_blockhashes, rent},
};
use tokio::time::sleep;
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn test_devnet_monitoring_clock_sysvar_changes() {
    // Create account updates watcher
    let mut watcher =
        RemoteAccountUpdatesWatcher::new(RpcProviderConfig::devnet());
    let reader = RemoteAccountUpdatesReader::new(&watcher);
    // Run the watcher in a separate task
    let cancellation_token = CancellationToken::new();
    let watcher_handle = {
        let cancellation_token = cancellation_token.clone();
        tokio::spawn(async move {
            watcher.start_monitoring(cancellation_token).await
        })
    };
    // Start monitoring the clock
    let sysvar_clock = clock::ID;
    assert!(!reader.has_known_update_since_slot(&sysvar_clock, 0));
    reader.request_account_monitoring(&sysvar_clock);
    // Wait for a few slots to happen on-chain
    sleep(Duration::from_millis(3000)).await;
    // Check that we detected the clock change
    assert!(reader.has_known_update_since_slot(&sysvar_clock, 0));
    // Cleanup everything correctly
    cancellation_token.cancel();
    assert!(watcher_handle.await.is_ok());
}
