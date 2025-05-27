use std::time::Duration;

use futures::StreamExt;
use solana_rpc_client_api::config::{
    RpcTransactionLogsConfig, RpcTransactionLogsFilter,
};
use solana_sdk::signer::Signer;
use test_pubsub::PubSubEnv;

#[tokio::test]
async fn test_logs_subscribe_all() {
    const TRANSFER_AMOUNT: u64 = 10_000;
    let env = PubSubEnv::new().await;

    let (mut rx, cancel) = env
        .ws_client
        .logs_subscribe(
            RpcTransactionLogsFilter::All,
            RpcTransactionLogsConfig { commitment: None },
        )
        .await
        .expect("failed to subscribe to txn logs");
    for _ in 0..5 {
        let signature = env.transfer(TRANSFER_AMOUNT).await.to_string();

        let update = rx
            .next()
            .await
            .expect("failed to receive signature update after tranfer txn");
        assert_eq!(
            update.value.signature, signature,
            "should have received executed transaction log"
        );
        assert!(update.value.err.is_none());
        assert!(!update.value.logs.is_empty());
        tokio::time::sleep(Duration::from_millis(100)).await
    }

    cancel().await;
    assert_eq!(
        rx.next().await,
        None,
        "txn logs subscription should have been cancelled properly"
    );
}

#[tokio::test]
async fn test_logs_subscribe_mentions() {
    const TRANSFER_AMOUNT: u64 = 10_000;
    let env = PubSubEnv::new().await;

    let (mut rx1, cancel1) = env
        .ws_client
        .logs_subscribe(
            RpcTransactionLogsFilter::Mentions(vec![env
                .account1
                .pubkey()
                .to_string()]),
            RpcTransactionLogsConfig { commitment: None },
        )
        .await
        .expect("failed to subscribe to txn logs for account 1");
    let (mut rx2, cancel2) = env
        .ws_client
        .logs_subscribe(
            RpcTransactionLogsFilter::Mentions(vec![env
                .account2
                .pubkey()
                .to_string()]),
            RpcTransactionLogsConfig { commitment: None },
        )
        .await
        .expect("failed to subscribe to txn logs for account 2");
    let sinature = env.transfer(TRANSFER_AMOUNT).await.to_string();
    for rx in [&mut rx1, &mut rx2] {
        let update = rx
            .next()
            .await
            .expect("failed to receive signature update after tranfer txn");
        assert_eq!(
            update.value.signature, sinature,
            "should have received executed transaction log"
        );
        assert!(update.value.err.is_none());
        assert!(!update.value.logs.is_empty());
    }

    cancel1().await;
    cancel2().await;
    assert_eq!(
        rx1.next().await,
        None,
        "txn logs subscription should have been cancelled properly"
    );
    assert_eq!(
        rx2.next().await,
        None,
        "txn logs subscription should have been cancelled properly"
    );
}
