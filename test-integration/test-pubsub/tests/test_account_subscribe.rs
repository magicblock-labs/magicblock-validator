use std::time::Duration;

use futures::StreamExt;
use solana_sdk::{native_token::LAMPORTS_PER_SOL, signer::Signer};
use test_pubsub::PubSubEnv;

macro_rules! expect_account_lamports_update {
    ($rx:expr, $expected:expr, $($allowed_stale:expr),* $(,)?) => {{
        const MAX_ATTEMPTS: usize = 50;
        let mut attempts = 0;
        let mut last_seen_lamports = None;
        loop {
            attempts += 1;
            assert!(
                attempts <= MAX_ATTEMPTS,
                "exceeded {} attempts waiting for lamports={}, last seen {:?}",
                MAX_ATTEMPTS,
                $expected,
                last_seen_lamports,
            );
            let update = tokio::time::timeout(
                Duration::from_millis(100),
                $rx.next(),
            )
            .await
            .expect("timeout waiting for account update")
            .expect("failed to receive account update after balance change");
            last_seen_lamports = Some(update.value.lamports);

            if update.value.lamports == $expected {
                break update;
            }

            let allowed_stale = [$($allowed_stale),*];
            assert!(
                allowed_stale.contains(&update.value.lamports),
                "unexpected account update lamports: got {}, expected {} or one of {:?}",
                update.value.lamports,
                $expected,
                allowed_stale,
            );
        }
    }};
}

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
    let update = expect_account_lamports_update!(
        &mut rx1,
        LAMPORTS_PER_SOL - TRANSFER_AMOUNT,
        LAMPORTS_PER_SOL,
    );
    assert_eq!(
        update.value.lamports,
        LAMPORTS_PER_SOL - TRANSFER_AMOUNT,
        "account 1 should have its balance decreased"
    );

    let update = expect_account_lamports_update!(
        &mut rx2,
        LAMPORTS_PER_SOL + TRANSFER_AMOUNT,
        LAMPORTS_PER_SOL,
    );
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
        let update = expect_account_lamports_update!(
            &mut rx1,
            LAMPORTS_PER_SOL - (i + 1) * TRANSFER_AMOUNT,
            LAMPORTS_PER_SOL - i * TRANSFER_AMOUNT,
        );
        assert_eq!(
            update.value.lamports,
            LAMPORTS_PER_SOL - (i + 1) * TRANSFER_AMOUNT,
            "account 1 should have its balance decreased"
        );
        // wait for blockhash to renew
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}
