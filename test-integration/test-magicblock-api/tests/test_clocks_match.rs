use std::time::Duration;

use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_sdk::{
    native_token::LAMPORTS_PER_SOL, pubkey::Pubkey, signature::Keypair,
    signer::Signer, system_instruction, transaction::Transaction,
};
use solana_transaction_status_client_types::UiTransactionEncoding;

/// Test that verifies transaction timestamps, block timestamps, and ledger block timestamps all match
#[tokio::test]
async fn test_clocks_match() {
    let iterations = 10;
    let millis_per_slot = 50;

    let from_keypair = Keypair::new();
    let to_pubkey = Pubkey::new_unique();

    let rpc_client = RpcClient::new("http://localhost:7849".to_string());
    rpc_client
        .request_airdrop(&from_keypair.pubkey(), LAMPORTS_PER_SOL)
        .await
        .unwrap();

    // Wait a moment for the validator to initialize
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Test multiple slots to ensure consistency
    for _ in 0..iterations {
        let transfer_tx = Transaction::new_signed_with_payer(
            &[system_instruction::transfer(
                &from_keypair.pubkey(),
                &to_pubkey,
                1000000,
            )],
            Some(&from_keypair.pubkey()),
            &[&from_keypair],
            rpc_client.get_latest_blockhash().await.unwrap(),
        );

        let tx_result = rpc_client
            .send_and_confirm_transaction(&transfer_tx)
            .await
            .unwrap();

        let mut tx = rpc_client
            .get_transaction(&tx_result, UiTransactionEncoding::Base64)
            .await
            .unwrap();
        // Wait until we're sure the slot is written to the ledger
        while rpc_client.get_slot().await.unwrap() < tx.slot + 2 {
            tx = rpc_client
                .get_transaction(&tx_result, UiTransactionEncoding::Base64)
                .await
                .unwrap();
            tokio::time::sleep(Duration::from_millis(millis_per_slot)).await;
        }

        let tx_timestamp = tx.block_time.unwrap();

        let ledger_timestamp =
            rpc_client.get_block_time(tx.slot).await.unwrap();
        let block_timestamp = rpc_client
            .get_block(tx.slot)
            .await
            .unwrap()
            .block_time
            .unwrap();

        // Verify timestamps match
        assert_eq!(
            block_timestamp, ledger_timestamp,
            "Timestamps should match for slot {}",
            tx.slot
        );
        assert_eq!(
            tx_timestamp, ledger_timestamp,
            "Timestamps should match for slot {}",
            tx.slot
        );

        // Also verify that the timestamp is not 0 and not in the future
        assert!(tx_timestamp > 0, "Timestamp should be positive");
        let current_time = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        assert!(
            tx_timestamp <= current_time,
            "Timestamp should be in the past: {} > {}",
            tx_timestamp,
            current_time
        );
    }
}
