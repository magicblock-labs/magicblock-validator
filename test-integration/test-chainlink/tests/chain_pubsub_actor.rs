use magicblock_chainlink::{
    remote_account_provider::pubsub_common::SubscriptionUpdate,
    testing::{
        chain_pubsub::{
            recycle, setup_actor_and_client, shutdown, subscribe, unsubscribe,
        },
        utils::{airdrop, init_logger, random_pubkey},
    },
};
use solana_pubkey::Pubkey;
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use tokio::{
    sync::mpsc,
    time::{timeout, Duration, Instant},
};

async fn expect_update_for(
    updates: &mut mpsc::Receiver<SubscriptionUpdate>,
    target: Pubkey,
) -> SubscriptionUpdate {
    loop {
        let maybe = timeout(Duration::from_millis(1500), updates.recv())
            .await
            .expect("timed out waiting for subscription update");
        let update = maybe.expect("subscription updates channel closed");
        if update.pubkey == target {
            return update;
        }
    }
}

async fn airdrop_and_expect_update(
    rpc_client: &RpcClient,
    updates: &mut mpsc::Receiver<SubscriptionUpdate>,
    pubkey: Pubkey,
    lamports: u64,
) -> SubscriptionUpdate {
    airdrop(rpc_client, &pubkey, lamports).await;
    expect_update_for(updates, pubkey).await
}

async fn expect_no_update_for(
    updates: &mut mpsc::Receiver<SubscriptionUpdate>,
    target: Pubkey,
    timeout_ms: u64,
) {
    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    loop {
        let now = Instant::now();
        if now >= deadline {
            break;
        }
        let remaining = deadline.saturating_duration_since(now);
        match timeout(remaining, updates.recv()).await {
            Ok(Some(update)) => {
                if update.pubkey == target {
                    panic!(
                        "unexpected update for unsubscribed account {target}"
                    );
                }
                // ignore other updates and keep waiting
            }
            Ok(None) => panic!("subscription updates channel closed"),
            Err(_) => break, // timed out => success
        }
    }
}

#[tokio::test]
async fn ixtest_recycle_connections() {
    init_logger();

    // 1. Create actor and RPC client with confirmed commitment
    let (actor, mut updates_rx, rpc_client) = setup_actor_and_client().await;

    // 2. Create account via airdrop
    let pubkey = random_pubkey();
    airdrop(&rpc_client, &pubkey, 1_000_000).await;

    // 3. Subscribe to that account
    subscribe(&actor, pubkey).await;

    // 4. Airdrop again and ensure we receive the update
    let _first_update = airdrop_and_expect_update(
        &rpc_client,
        &mut updates_rx,
        pubkey,
        2_000_000,
    )
    .await;

    // 5. Recycle connections
    recycle(&actor).await;

    // 6. Airdrop again and ensure we receive the update again
    let _second_update = airdrop_and_expect_update(
        &rpc_client,
        &mut updates_rx,
        pubkey,
        3_000_000,
    )
    .await;

    // Cleanup
    shutdown(&actor).await;
}

#[tokio::test]
async fn ixtest_recycle_connections_multiple_accounts() {
    init_logger();

    // Setup
    let (actor, mut updates_rx, rpc_client) = setup_actor_and_client().await;

    // Create 4 accounts and fund them once to ensure existence
    let pks = [
        random_pubkey(),
        random_pubkey(),
        random_pubkey(),
        random_pubkey(),
    ];
    for pk in &pks {
        airdrop(&rpc_client, pk, 1_000_000).await;
    }

    // Subscribe to all 4
    for &pk in &pks {
        subscribe(&actor, pk).await;
    }

    // Airdrop to each and ensure we receive updates for all
    for &pk in &pks {
        let _ = airdrop_and_expect_update(
            &rpc_client,
            &mut updates_rx,
            pk,
            2_000_000,
        )
        .await;
    }

    // Unsubscribe from the 4th
    let unsub_pk = pks[3];
    unsubscribe(&actor, unsub_pk).await;

    // Recycle connections
    recycle(&actor).await;

    // Airdrop to first three and expect updates
    for &pk in &pks[0..3] {
        let _ = airdrop_and_expect_update(
            &rpc_client,
            &mut updates_rx,
            pk,
            3_000_000,
        )
        .await;
    }

    // Airdrop to the 4th and ensure we do NOT receive an update for it
    airdrop(&rpc_client, &unsub_pk, 3_000_000).await;
    expect_no_update_for(&mut updates_rx, unsub_pk, 1500).await;

    // Cleanup
    shutdown(&actor).await;
}
