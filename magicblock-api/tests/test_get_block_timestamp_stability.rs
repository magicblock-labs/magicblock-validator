use std::time::Duration;

use magicblock_api::{
    errors::ApiResult,
    magic_validator::{MagicValidator, MagicValidatorConfig},
    InitGeyserServiceConfig,
};
use magicblock_config::LifecycleMode;
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_sdk::signature::Keypair;
use tempfile::TempDir;

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
