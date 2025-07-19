use std::time::Duration;

use solana_rpc_client::nonblocking::rpc_client::RpcClient;

#[tokio::test]
async fn test_get_block_timestamp_stability() {
    let millis_per_slot = 50;

    // Wait for a few slots to pass
    let skipped_slots = 10;
    tokio::time::sleep(Duration::from_millis(
        100 + millis_per_slot * skipped_slots, // 100ms to start the validator
    ))
    .await;

    let rpc_client = RpcClient::new("http://localhost:7849".to_string());

    let current_slot = rpc_client.get_slot().await.unwrap();
    let block_time = rpc_client.get_block_time(current_slot - 1).await.unwrap();
    let ledger_block_time = rpc_client
        .get_block(current_slot - 1)
        .await
        .unwrap()
        .block_time
        .unwrap();

    assert_eq!(block_time, ledger_block_time);
}
