use log::*;
use std::time::Duration;
use test_kit::init_logger;

use integration_test_tools::IntegrationTestContext;
use solana_sdk::{
    native_token::LAMPORTS_PER_SOL, signature::Keypair, signer::Signer,
    system_instruction,
};

/// Test that verifies transaction timestamps, block timestamps, and ledger block timestamps all match
#[test]
fn test_clocks_match() {
    init_logger!();

    let iterations = 10;
    let millis_per_slot = 50;

    let chain_payer = Keypair::new();
    let from_keypair = Keypair::new();
    let to_keypair = Keypair::new();

    let ctx = IntegrationTestContext::try_new().unwrap();
    ctx.airdrop_chain(&chain_payer.pubkey(), 10 * LAMPORTS_PER_SOL)
        .unwrap();
    ctx.airdrop_chain_and_delegate(
        &chain_payer,
        &from_keypair,
        LAMPORTS_PER_SOL,
    )
    .unwrap();
    ctx.airdrop_chain_and_delegate(&chain_payer, &to_keypair, LAMPORTS_PER_SOL)
        .unwrap();

    debug!(
        "✅ Airdropped and delegated from {} and to {}",
        from_keypair.pubkey(),
        to_keypair.pubkey()
    );

    // Test multiple slots to ensure consistency
    for _ in 0..iterations {
        let (sig, confirmed) = ctx
            .send_and_confirm_instructions_with_payer_ephem(
                &[system_instruction::transfer(
                    &from_keypair.pubkey(),
                    &to_keypair.pubkey(),
                    1000000,
                )],
                &from_keypair,
            )
            .unwrap();
        debug!("✅ Transfer tx {sig} confirmed: {confirmed}");
        assert!(confirmed);

        let mut tx = ctx.get_transaction_ephem(&sig).unwrap();
        // Wait until we're sure the slot is written to the ledger
        while ctx.get_slot_ephem().unwrap() < tx.slot + 10 {
            tx = ctx.get_transaction_ephem(&sig).unwrap();
            std::thread::sleep(Duration::from_millis(millis_per_slot));
        }
        debug!(
            "✅ Transaction {} with slot {} and block time {:?} written to ledger",
            sig, tx.slot, tx.block_time
        );

        let ledger_timestamp = ctx.try_get_block_time_ephem(tx.slot).unwrap();
        let block_timestamp = ctx.try_get_block_ephem(tx.slot).unwrap();
        let block_timestamp = block_timestamp.block_time;

        debug!(
            "Ledger block time for slot {} is {:?}, block time is {:?}",
            tx.slot, ledger_timestamp, block_timestamp
        );

        // Verify timestamps match
        assert_eq!(
            block_timestamp,
            Some(ledger_timestamp),
            "Timestamps should match for slot {}",
            tx.slot,
        );
        assert_eq!(
            tx.block_time,
            Some(ledger_timestamp),
            "Timestamps should match for slot {}: {:?} != {:?}",
            tx.slot,
            tx.block_time,
            Some(ledger_timestamp),
        );

        // Also verify that the timestamp is not 0 and not in the future
        assert!(
            tx.block_time.map(|t| t > 0).unwrap_or_default(),
            "Timestamp should be positive",
        );
        let current_time = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        assert!(
            tx.block_time.map(|t| t <= current_time).unwrap_or_default(),
            "Timestamp should be in the past: {:?} > {}",
            tx.block_time,
            current_time,
        );
    }
}
