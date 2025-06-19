use std::time::Duration;

use futures::StreamExt;
use solana_rpc_client_api::response::{
    ProcessedSignatureResult, RpcSignatureResult,
};
use test_pubsub::PubSubEnv;

#[tokio::test]
async fn test_signature_subscribe() {
    const TRANSFER_AMOUNT: u64 = 10_000;
    let env = PubSubEnv::new().await;
    let txn = env.transfer_txn(TRANSFER_AMOUNT).await;
    let signature = txn.signatures.first().unwrap();

    let (mut rx, cancel) = env
        .ws_client
        .signature_subscribe(signature, None)
        .await
        .expect("failed to subscribe to signature");
    env.send_txn(txn).await;

    let update = rx
        .next()
        .await
        .expect("failed to receive signature update after tranfer txn");
    assert_eq!(
        update.value,
        RpcSignatureResult::ProcessedSignature(ProcessedSignatureResult {
            err: None
        })
    );

    cancel().await;
    assert_eq!(
        rx.next().await,
        None,
        "signature subscription should have been cancelled properly"
    );
}

#[tokio::test]
async fn test_signature_subscribe_with_delay() {
    const TRANSFER_AMOUNT: u64 = 10_000;
    let env = PubSubEnv::new().await;
    let signature = env.transfer(TRANSFER_AMOUNT).await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    let (mut rx, cancel) = env
        .ws_client
        .signature_subscribe(&signature, None)
        .await
        .expect("failed to subscribe to signature");

    let update = rx
        .next()
        .await
        .expect("failed to receive signature update after tranfer txn");
    assert_eq!(
        update.value,
        RpcSignatureResult::ProcessedSignature(ProcessedSignatureResult {
            err: None
        })
    );

    cancel().await;
    assert_eq!(
        rx.next().await,
        None,
        "signature subscription should have been cancelled properly"
    );
}
