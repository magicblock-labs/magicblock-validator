use std::time::Duration;

use cleanass::assert_eq;
use integration_test_tools::{expect, loaded_accounts::LoadedAccounts};
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use test_magicblock_api::{cleanup, start_magicblock_validator_with_config};

const EPHEM_URL: &str = "http://localhost:8899";

#[tokio::test]
async fn test_get_block_timestamp_stability() {
    let mut validator = start_magicblock_validator_with_config(
        "validator-api-offline.devnet.toml",
        &LoadedAccounts::with_delegation_program_test_authority(),
    );

    let millis_per_slot = 50;

    // Wait for a few slots to pass
    let skipped_slots = 10;
    tokio::time::sleep(Duration::from_millis(
        100 + millis_per_slot * skipped_slots, // 100ms to start the validator
    ))
    .await;

    let rpc_client = RpcClient::new(EPHEM_URL.to_string());

    let current_slot = expect!(rpc_client.get_slot().await, validator);
    let block_time =
        expect!(rpc_client.get_block_time(current_slot - 1).await, validator);
    let ledger_block =
        expect!(rpc_client.get_block(current_slot - 1).await, validator);

    assert_eq!(
        ledger_block.block_time,
        Some(block_time),
        cleanup(&mut validator)
    );

    cleanup(&mut validator);
}
