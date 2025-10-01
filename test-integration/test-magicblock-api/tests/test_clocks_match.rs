use std::time::Duration;

use integration_test_tools::IntegrationTestContext;
use solana_rpc_client::rpc_client::RpcClient;
use solana_sdk::{
    native_token::LAMPORTS_PER_SOL, signature::Keypair, signer::Signer,
    system_instruction, transaction::Transaction,
};
use solana_transaction_status::UiTransactionEncoding;

const EPHEM_URL: &str = "http://localhost:8899";

/// Test that verifies transaction timestamps, block timestamps, and ledger block timestamps all match
#[test]
fn test_clocks_match() {
    let iterations = 10;
    let millis_per_slot = 50;
    let ctx = IntegrationTestContext::try_new().unwrap();

    let payer_chain = Keypair::new();
    let from_keypair = Keypair::new();
    let to_keypair = Keypair::new();
    // Fund payer on chain which will fund accounts we delegate
    ctx.airdrop_chain(&payer_chain.pubkey(), 5 * LAMPORTS_PER_SOL)
        .unwrap();
    ctx.airdrop_chain_and_delegate(
        &payer_chain,
        &from_keypair,
        LAMPORTS_PER_SOL,
    )
    .unwrap();
    ctx.airdrop_chain_and_delegate(&payer_chain, &to_keypair, LAMPORTS_PER_SOL)
        .unwrap();

    let rpc_client = RpcClient::new(EPHEM_URL.to_string());

    // Test multiple slots to ensure consistency
    for _ in 0..iterations {
        let blockhash = rpc_client.get_latest_blockhash().unwrap();
        let transfer_tx = Transaction::new_signed_with_payer(
            &[system_instruction::transfer(
                &from_keypair.pubkey(),
                &to_keypair.pubkey(),
                1000000,
            )],
            Some(&from_keypair.pubkey()),
            &[&from_keypair],
            blockhash,
        );

        let tx_result = rpc_client
            .send_and_confirm_transaction(&transfer_tx)
            .unwrap();

        let mut tx = rpc_client
            .get_transaction(&tx_result, UiTransactionEncoding::Base64)
            .unwrap();
        // Wait until we're sure the slot is written to the ledger
        while rpc_client.get_slot().unwrap() < tx.slot + 10 {
            tx = rpc_client
                .get_transaction(&tx_result, UiTransactionEncoding::Base64)
                .unwrap();
            std::thread::sleep(Duration::from_millis(millis_per_slot));
        }

        let ledger_timestamp = rpc_client.get_block_time(tx.slot).unwrap();
        let block_timestamp = rpc_client.get_block(tx.slot).unwrap();
        let block_timestamp = block_timestamp.block_time;

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
