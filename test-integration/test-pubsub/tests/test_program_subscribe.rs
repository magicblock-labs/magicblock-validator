use std::time::Duration;

use futures::StreamExt;
use solana_sdk::{
    native_token::LAMPORTS_PER_SOL, pubkey::Pubkey, signer::Signer,
};
use test_pubsub::PubSubEnv;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_program_subscribe() {
    let env = PubSubEnv::new().await;
    let (mut rx, cancel) = env
        .ws_client
        .program_subscribe(&Pubkey::default(), None)
        .await
        .expect("failed to subscribe to program");

    const TRANSFER_AMOUNT: u64 = 10_000;
    env.transfer(TRANSFER_AMOUNT);
    let expected_lamports1 = LAMPORTS_PER_SOL - TRANSFER_AMOUNT;
    let expected_lamports2 = LAMPORTS_PER_SOL + TRANSFER_AMOUNT;
    for _ in 0..2 {
        // We ignore all updates for the accounts like them being cloned
        // until we see the balance changes
        let update = loop {
            let update = tokio::time::timeout(
                Duration::from_millis(100),
                rx.next(),
            )
            .await
            .expect("timeout waiting for program update")
            .expect("failed to receive accounts update after balance change");

            if (update.value.pubkey == env.account1.pubkey().to_string()
                && update.value.account.lamports == expected_lamports1)
                || (update.value.pubkey == env.account2.pubkey().to_string()
                    && update.value.account.lamports == expected_lamports2)
            {
                break update;
            }
        };
        if update.value.pubkey == env.account1.pubkey().to_string() {
            assert_eq!(
                update.value.account.lamports, expected_lamports1,
                "account 1 should have its balance decreased"
            );
        } else {
            assert_eq!(
                update.value.account.lamports, expected_lamports2,
                "account 2 should have its balance increased"
            );
        }
    }
    cancel().await;
    assert_eq!(
        rx.next().await,
        None,
        "program subscription should have been cancelled properly"
    );
}
