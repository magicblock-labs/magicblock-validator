use std::time::Duration;

use magicblock_api::{
    errors::ApiResult,
    magic_validator::{MagicValidator, MagicValidatorConfig},
    InitGeyserServiceConfig,
};
use magicblock_config::LifecycleMode;
use magicblock_processor::execute_transaction::execute_legacy_transaction;
use magicblock_transaction_status::TransactionStatusMeta;
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_sdk::{
    pubkey::Pubkey,
    signature::Keypair,
    signer::Signer,
    system_instruction,
    transaction::{SanitizedTransaction, Transaction},
};
use tempfile::TempDir;

/// Test that verifies transaction timestamps, block timestamps, and ledger block timestamps all match
#[tokio::test]
async fn test_clocks_match() -> ApiResult<()> {
    // Initialize a temporary directory for the test
    let temp_dir = TempDir::new()?;
    let ledger_path = temp_dir.path().join("ledger");

    // Create a simple configuration for the validator using defaults
    let millis_per_slot = 200; // 200ms per slot
    let mut config = magicblock_config::EphemeralConfig::default();
    config.accounts.lifecycle = LifecycleMode::Offline;
    config.validator.millis_per_slot = millis_per_slot;
    config.ledger.path = Some(ledger_path.to_string_lossy().to_string());
    config.ledger.reset = true;
    let validator_config = MagicValidatorConfig {
        validator_config: config.clone(),
        init_geyser_service_config: InitGeyserServiceConfig::default(),
    };

    // Create and start the validator
    let identity_keypair = Keypair::new();
    let mut validator = MagicValidator::try_from_config(
        validator_config,
        identity_keypair.insecure_clone(),
    )?;
    validator.start().await?;

    // Get references to the bank and ledger
    let bank = validator.bank_rc();
    let ledger = validator.ledger();

    // Wait a moment for the validator to initialize
    tokio::time::sleep(Duration::from_millis(millis_per_slot / 2)).await;

    // Test multiple slots to ensure consistency
    for _ in 0..10 {
        let current_slot = bank.slot();

        // Create another transaction
        let from_keypair = Keypair::new();
        let to_pubkey = Pubkey::new_unique();
        let instruction = system_instruction::transfer(
            &from_keypair.pubkey(),
            &to_pubkey,
            100,
        );
        let transaction = Transaction::new_signed_with_payer(
            &[instruction],
            Some(&from_keypair.pubkey()),
            &[&from_keypair],
            bank.last_blockhash(),
        );

        // Fund the account
        let fund_instruction = system_instruction::transfer(
            &identity_keypair.pubkey(),
            &from_keypair.pubkey(),
            1000,
        );
        let fund_transaction = Transaction::new_signed_with_payer(
            &[fund_instruction],
            Some(&identity_keypair.pubkey()),
            &[&identity_keypair],
            bank.last_blockhash(),
        );

        let fund_tx =
            execute_legacy_transaction(fund_transaction.clone(), &bank, None)
                .unwrap();
        let tx_result =
            execute_legacy_transaction(transaction.clone(), &bank, None)
                .unwrap();

        ledger
            .write_transaction(
                fund_tx,
                current_slot,
                SanitizedTransaction::from_transaction_for_tests(
                    fund_transaction,
                ),
                TransactionStatusMeta::default(),
                current_slot as usize,
            )
            .unwrap();
        ledger
            .write_transaction(
                tx_result,
                current_slot,
                SanitizedTransaction::from_transaction_for_tests(transaction),
                TransactionStatusMeta::default(),
                current_slot as usize,
            )
            .unwrap();

        // Wait for processing
        tokio::time::sleep(Duration::from_millis(millis_per_slot * 110 / 100))
            .await;

        let current_slot = bank.slot();
        let current_timestamp = bank.clock().unix_timestamp;

        // Get the block timestamp from ledger
        let block = ledger.get_block(current_slot - 1).unwrap().unwrap();
        let ledger_timestamp = block.block_time.unwrap();

        let transaction_timestamp = ledger
            .get_complete_transaction(tx_result, current_slot - 1)
            .unwrap()
            .unwrap()
            .block_time
            .unwrap();

        // Verify timestamps match
        assert_eq!(
            current_timestamp,
            ledger_timestamp,
            "Timestamps should match for slot {}",
            current_slot - 1
        );
        assert_eq!(
            current_timestamp,
            transaction_timestamp,
            "Timestamps should match for slot {}",
            current_slot - 1
        );

        // All timestamps should match
        assert_eq!(
            transaction_timestamp, ledger_timestamp,
            "Transaction timestamp should match ledger block timestamp"
        );

        // Also verify that the timestamp is reasonable (not 0 and not in the future)
        assert!(transaction_timestamp > 0, "Timestamp should be positive");
        let current_time = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        assert!(
            transaction_timestamp <= current_time,
            "Timestamp should be in the past: {} > {}",
            transaction_timestamp,
            current_time
        );
    }

    tokio::task::spawn_blocking(move || drop(validator))
        .await
        .unwrap();

    Ok(())
}

#[tokio::test]
async fn test_get_block_timestamp_stability() -> ApiResult<()> {
    // Initialize a temporary directory for the test
    let temp_dir = TempDir::new()?;
    let ledger_path = temp_dir.path().join("ledger");

    // Create a simple configuration for the validator using defaults
    let millis_per_slot = 1000; // 1 second per slot
    let mut config = magicblock_config::EphemeralConfig::default();
    config.accounts.lifecycle = LifecycleMode::Offline;
    config.validator.millis_per_slot = millis_per_slot;
    config.ledger.path = Some(ledger_path.to_string_lossy().to_string());
    config.ledger.reset = true;
    let validator_config = MagicValidatorConfig {
        validator_config: config.clone(),
        init_geyser_service_config: InitGeyserServiceConfig::default(),
    };

    // Create and start the validator
    let identity_keypair = Keypair::new();
    let mut validator = MagicValidator::try_from_config(
        validator_config,
        identity_keypair.insecure_clone(),
    )?;
    validator.start().await?;

    // Get references to the bank and ledger
    let bank = validator.bank_rc();
    let ledger = validator.ledger();

    // Wait for a few slots to pass
    let skipped_slots = 10;
    tokio::time::sleep(Duration::from_millis(
        100 + millis_per_slot * skipped_slots,
    ))
    .await;

    let rpc_client =
        RpcClient::new(format!("http://{}", config.rpc.socket_addr()));

    let current_slot = bank.slot();
    let block_time = rpc_client.get_block_time(current_slot - 1).await.unwrap();
    let ledger_block_time = ledger
        .get_block(current_slot - 1)
        .unwrap()
        .unwrap()
        .block_time
        .unwrap();

    assert_eq!(block_time, ledger_block_time);

    tokio::task::spawn_blocking(move || drop(validator))
        .await
        .unwrap();

    Ok(())
}
