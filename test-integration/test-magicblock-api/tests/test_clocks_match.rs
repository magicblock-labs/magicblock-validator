use std::time::Duration;

use cleanass::{assert, assert_eq};
use integration_test_tools::{expect, loaded_accounts::LoadedAccounts};
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_sdk::{
    native_token::LAMPORTS_PER_SOL, pubkey::Pubkey, signature::Keypair,
    signer::Signer, system_instruction, transaction::Transaction,
};
use solana_transaction_status_client_types::UiTransactionEncoding;
use test_magicblock_api::{cleanup, start_magicblock_validator_with_config};

const EPHEM_URL: &str = "http://localhost:8899";

/// Test that verifies transaction timestamps, block timestamps, and ledger block timestamps all match
#[tokio::test]
async fn test_clocks_match() {
    let mut validator = start_magicblock_validator_with_config(
        "validator-api-offline.devnet.toml",
        &LoadedAccounts::with_delegation_program_test_authority(),
    );

    let iterations = 10;
    let millis_per_slot = 50;

    let from_keypair = Keypair::new();
    let to_pubkey = Pubkey::new_unique();

    let rpc_client = RpcClient::new(EPHEM_URL.to_string());
    expect!(
        rpc_client
            .request_airdrop(&from_keypair.pubkey(), LAMPORTS_PER_SOL)
            .await,
        validator
    );

    // Wait a moment for the validator to initialize
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Test multiple slots to ensure consistency
    for _ in 0..iterations {
        let blockhash =
            expect!(rpc_client.get_latest_blockhash().await, validator);
        let transfer_tx = Transaction::new_signed_with_payer(
            &[system_instruction::transfer(
                &from_keypair.pubkey(),
                &to_pubkey,
                1000000,
            )],
            Some(&from_keypair.pubkey()),
            &[&from_keypair],
            blockhash,
        );

        let tx_result = expect!(
            rpc_client.send_and_confirm_transaction(&transfer_tx).await,
            validator
        );

        let mut tx = expect!(
            rpc_client
                .get_transaction(&tx_result, UiTransactionEncoding::Base64)
                .await,
            validator
        );
        // Wait until we're sure the slot is written to the ledger
        while expect!(rpc_client.get_slot().await, validator) < tx.slot + 10 {
            tx = expect!(
                rpc_client
                    .get_transaction(&tx_result, UiTransactionEncoding::Base64)
                    .await,
                validator
            );
            tokio::time::sleep(Duration::from_millis(millis_per_slot)).await;
        }

        let ledger_timestamp =
            expect!(rpc_client.get_block_time(tx.slot).await, validator);
        let block_timestamp =
            expect!(rpc_client.get_block(tx.slot).await, validator);
        let block_timestamp = block_timestamp.block_time;

        // Verify timestamps match
        assert_eq!(
            block_timestamp,
            Some(ledger_timestamp),
            cleanup(&mut validator),
            "Timestamps should match for slot {}",
            tx.slot,
        );
        assert_eq!(
            tx.block_time,
            Some(ledger_timestamp),
            cleanup(&mut validator),
            "Timestamps should match for slot {}: {:?} != {:?}",
            tx.slot,
            tx.block_time,
            Some(ledger_timestamp),
        );

        // Also verify that the timestamp is not 0 and not in the future
        assert!(
            tx.block_time.map(|t| t > 0).unwrap_or(false),
            cleanup(&mut validator),
            "Timestamp should be positive",
        );
        let current_time = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        assert!(
            tx.block_time.map(|t| t <= current_time).unwrap_or(false),
            cleanup(&mut validator),
            "Timestamp should be in the past: {:?} > {}",
            tx.block_time,
            current_time,
        );
    }

    cleanup(&mut validator);
}
