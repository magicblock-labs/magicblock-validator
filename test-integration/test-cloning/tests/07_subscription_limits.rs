use std::{sync::Arc, time::Duration};

use integration_test_tools::IntegrationTestContext;
use log::*;
use solana_sdk::{
    native_token::LAMPORTS_PER_SOL, rent::Rent, signature::Keypair,
    signer::Signer,
};
use test_kit::init_logger;
use tokio::task::JoinSet;

const NUM_PUBKEYS: usize = 400;
// Half of the accounts are delegated and aren't watched
const EXTRA_MONITORED_ACCOUNTS: usize = NUM_PUBKEYS / 2;
const AIRDROP_CHUNK_SIZE: usize = 100;
// See metrics config in: configs/cloning-conf.ephem.toml
const PORT: u16 = 9000;

// This test creates a large number of accounts, airdrops to all of them
// and delegates half.
// It then ensures that the subscription count increased as expected.
// Since it will be affected by other tests that trigger subscriptions,
// we only run it in isolation manually.
#[ignore = "Run manually only"]
#[tokio::test(flavor = "multi_thread")]
async fn test_large_number_of_account_subscriptions() {
    init_logger!();
    let ctx = Arc::new(IntegrationTestContext::try_new().unwrap());

    debug!("Generating {NUM_PUBKEYS} keypairs...");
    let keypairs: Vec<Keypair> =
        (0..NUM_PUBKEYS).map(|_| Keypair::new()).collect();
    debug!("✅ Generated {NUM_PUBKEYS} keypairs");

    // TODO: need to delegate half those instead as part of airdropping
    // that way we can test unsub
    let rent_exempt_amount = Rent::default().minimum_balance(0);
    debug!(
        "Airdropping {rent_exempt_amount} lamports to {NUM_PUBKEYS} accounts in chunks of {AIRDROP_CHUNK_SIZE}..."
    );

    let payer_chain = Keypair::new();
    ctx.airdrop_chain(&payer_chain.pubkey(), LAMPORTS_PER_SOL * 10)
        .expect("failed to airdrop to payer_chain");

    let monitored_accounts_before =
        ctx.get_monitored_accounts_count(PORT).unwrap();
    let mut total_processed = 0;
    for (chunk_idx, chunk) in keypairs.chunks(AIRDROP_CHUNK_SIZE).enumerate() {
        let mut join_set = JoinSet::new();
        for (idx, keypair) in chunk.iter().enumerate() {
            let keypair = keypair.insecure_clone();
            let payer_chain = payer_chain.insecure_clone();
            let ctx = ctx.clone();
            join_set.spawn(async move {
                if idx % 2 == 0 {
                    ctx.airdrop_chain_and_delegate(
                        &payer_chain,
                        &keypair,
                        rent_exempt_amount,
                    )
                    .expect(
                        "failed to airdrop and delegate to on-chain account",
                    );
                } else {
                    ctx.airdrop_chain(&keypair.pubkey(), rent_exempt_amount)
                        .expect("failed to airdrop to on-chain account");
                }
            });
        }
        for _result in join_set.join_all().await {
            // spawned task panicked or was cancelled - handled by join_all
        }
        total_processed += chunk.len();

        let pubkeys = chunk.iter().map(|kp| kp.pubkey()).collect::<Vec<_>>();

        trace!(
            "Pubkeys in chunk {}: {}",
            chunk_idx + 1,
            pubkeys
                .iter()
                .map(|k| k.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        );

        debug!(
            "✅ Airdropped batch {}: {}/{} accounts ({} total)",
            chunk_idx + 1,
            chunk.len(),
            AIRDROP_CHUNK_SIZE,
            total_processed
        );

        let _accounts = ctx
            .fetch_ephem_multiple_accounts(&pubkeys)
            .expect("failed to fetch accounts");

        debug!(
            "✅ Fetched batch {}: {}/{} accounts ({} total)",
            chunk_idx + 1,
            chunk.len(),
            AIRDROP_CHUNK_SIZE,
            total_processed
        );
    }

    debug!("✅ Airdropped and fetched all {NUM_PUBKEYS} accounts from ephemeral RPC");

    // Wait 1 second for metrics update
    tokio::time::sleep(Duration::from_secs(5)).await;

    let monitored_accounts_after =
        ctx.get_monitored_accounts_count(PORT).unwrap();
    let diff = monitored_accounts_after - monitored_accounts_before;
    debug!("Monitored accounts count total: {monitored_accounts_after}, diff: {diff}");

    assert_eq!(
        diff, EXTRA_MONITORED_ACCOUNTS,
        "Expected monitored accounts to increase by {EXTRA_MONITORED_ACCOUNTS}"
    );
}
