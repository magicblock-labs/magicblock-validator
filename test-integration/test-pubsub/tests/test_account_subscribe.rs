use std::time::Duration;

use futures::StreamExt;
use solana_sdk::{native_token::LAMPORTS_PER_SOL, signer::Signer};
use test_pubsub::PubSubEnv;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_account_subscribe() {
    let env = PubSubEnv::new().await;
    let (mut rx1, cancel1) = env
        .ws_client
        .account_subscribe(&env.account1.pubkey(), None)
        .await
        .expect("failed to subscribe to account 1");
    let (mut rx2, cancel2) = env
        .ws_client
        .account_subscribe(&env.account2.pubkey(), None)
        .await
        .expect("failed to subscribe to account 2");

    const TRANSFER_AMOUNT: u64 = 10_000;
    env.transfer(TRANSFER_AMOUNT);
    let update = rx1
        .next()
        .await
        .expect("failed to receive account 1 update after balance change");
    assert_eq!(
        update.value.lamports, LAMPORTS_PER_SOL,
        "account 1 should have its initial update cached"
    );
    let update = rx1
        .next()
        .await
        .expect("failed to receive account 1 update after balance change");
    assert_eq!(
        update.value.lamports,
        LAMPORTS_PER_SOL - TRANSFER_AMOUNT,
        "account 1 should have its balance decreased"
    );

    let update = rx2
        .next()
        .await
        .expect("failed to receive account 2 update after balance change");
    assert_eq!(
        update.value.lamports, LAMPORTS_PER_SOL,
        "account 2 should have its initial update cached"
    );
    let update = rx2
        .next()
        .await
        .expect("failed to receive account 2 update after balance change");
    assert_eq!(
        update.value.lamports,
        LAMPORTS_PER_SOL + TRANSFER_AMOUNT,
        "account 2 should have its balance increased"
    );

    cancel1().await;
    cancel2().await;
    assert_eq!(
        rx1.next().await,
        None,
        "account 1 subscription should have been cancelled properly"
    );
    assert_eq!(
        rx2.next().await,
        None,
        "account 2 subscription should have been cancelled properly"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_account_subscribe_multiple_updates() {
    let env = PubSubEnv::new().await;
    let (mut rx1, _) = env
        .ws_client
        .account_subscribe(&env.account1.pubkey(), None)
        .await
        .expect("failed to subscribe to account 1");

    const TRANSFER_AMOUNT: u64 = 10_000;
    for i in 0..10 {
        env.transfer(TRANSFER_AMOUNT);
        let update = rx1
            .next()
            .await
            .expect("failed to receive account 1 update after balance change");
        assert_eq!(
            update.value.lamports,
            LAMPORTS_PER_SOL - i * TRANSFER_AMOUNT,
            "account 1 should have its balance decreased"
        );
        // wait for blockhash to renew
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}
