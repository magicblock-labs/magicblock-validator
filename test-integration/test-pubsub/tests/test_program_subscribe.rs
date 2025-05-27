use futures::StreamExt;
use solana_sdk::{
    native_token::LAMPORTS_PER_SOL, pubkey::Pubkey, signer::Signer,
};

use test_pubsub::PubSubEnv;

#[tokio::test]
async fn test_program_subscribe() {
    let env = PubSubEnv::new().await;
    let (mut rx, cancel) = env
        .ws_client
        .program_subscribe(&Pubkey::default(), None)
        .await
        .expect("failed to subscribe to program");

    const TRANSFER_AMOUNT: u64 = 10_000;
    env.transfer(TRANSFER_AMOUNT).await;
    for _ in 0..2 {
        let update = rx
            .next()
            .await
            .expect("failed to receive accounts update after balance change");
        if update.value.pubkey == env.account1.pubkey().to_string() {
            assert_eq!(
                update.value.account.lamports,
                LAMPORTS_PER_SOL - TRANSFER_AMOUNT,
                "account 1 should have its balance decreased"
            );
        } else {
            assert_eq!(
                update.value.account.lamports,
                LAMPORTS_PER_SOL + TRANSFER_AMOUNT,
                "account 1 should have its balance increased"
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
