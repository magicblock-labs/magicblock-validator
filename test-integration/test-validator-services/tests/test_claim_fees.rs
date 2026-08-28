use std::{thread::sleep, time::Duration};

use dlp_api::instruction_builder::validator_claim_fees;
use integration_test_tools::{
    loaded_accounts::LoadedAccounts, IntegrationTestContext,
};
use solana_commitment_config::CommitmentConfig;
use solana_rpc_client::rpc_client::RpcClient;
use solana_sdk::{
    pubkey::Pubkey,
    signature::{Keypair, Signer},
    transaction::Transaction,
};

// Test constants
const DEVNET_URL: &str = "http://127.0.0.1:7799";
const TEST_FEE_AMOUNT: u64 = 1_000_000;
const INITIAL_AIRDROP_AMOUNT: u64 = 5_000_000_000;
const CONFIRMATION_WAIT_MS: u64 = 500;
const SETUP_WAIT_MS: u64 = 1000;

fn validator_keypair() -> Keypair {
    let loaded_accounts =
        LoadedAccounts::with_delegation_program_test_authority();
    loaded_accounts
        .validator_authority_keypair()
        .insecure_clone()
}

fn validator_pubkey() -> Pubkey {
    LoadedAccounts::with_delegation_program_test_authority()
        .validator_authority()
}

fn validator_fees_vault() -> Pubkey {
    LoadedAccounts::with_delegation_program_test_authority()
        .validator_fees_vault()
}

/// Test that claim fees instruction
fn test_claim_fees_instruction() {
    println!("Testing claim fees instruction creation...");

    let validator_pubkey = validator_pubkey();
    eprintln!("validator_pubkey: {:?}", validator_pubkey);
    let instruction = validator_claim_fees(validator_pubkey, None);

    assert!(
        !instruction.accounts.is_empty(),
        "Instruction should have accounts"
    );
    assert_eq!(
        instruction.program_id,
        dlp_api::id(),
        "Instruction should target delegation program"
    );

    println!("✓ Claim fees instruction created successfully");
}

/// Add test fees to the vault
fn test_add_fees_to_vault() {
    println!("Adding test fees to vault...");

    let rpc_client = RpcClient::new_with_commitment(
        DEVNET_URL,
        CommitmentConfig::confirmed(),
    );

    let validator_fees_vault = validator_fees_vault();

    println!("  Target vault: {}", validator_fees_vault);

    rpc_client
        .request_airdrop(&validator_fees_vault, TEST_FEE_AMOUNT)
        .unwrap();
    sleep(Duration::from_millis(SETUP_WAIT_MS));

    let balance = rpc_client.get_balance(&validator_fees_vault).unwrap();
    assert!(
        balance >= TEST_FEE_AMOUNT,
        "Vault should have at least the test fee amount"
    );
    println!("✓ Added {} lamports test fees to vault", TEST_FEE_AMOUNT);
}

/// Test the actual fee claiming transaction
fn test_claim_fees_transaction() {
    println!("Testing actual claim fees transaction...");

    let rpc_client = RpcClient::new_with_commitment(
        DEVNET_URL,
        CommitmentConfig::confirmed(),
    );

    let validator_keypair = validator_keypair();
    let validator_pubkey = validator_keypair.pubkey();

    let validator_fees_vault = validator_fees_vault();

    println!("  Validator: {}", validator_pubkey);
    println!("  Fees vault: {}", validator_fees_vault);

    let balance_before = rpc_client.get_balance(&validator_fees_vault).unwrap();
    let instruction = validator_claim_fees(validator_pubkey, None);
    let blockhash = rpc_client.get_latest_blockhash().unwrap();
    let transaction = Transaction::new_signed_with_payer(
        &[instruction],
        Some(&validator_pubkey),
        &[&validator_keypair],
        blockhash,
    );

    rpc_client
        .send_and_confirm_transaction(&transaction)
        .unwrap();
    sleep(Duration::from_millis(CONFIRMATION_WAIT_MS));

    let balance_after = rpc_client.get_balance(&validator_fees_vault).unwrap();
    let vault_difference = balance_before.saturating_sub(balance_after);

    println!(
        "✓ Successfully claimed {} lamports in fees!",
        vault_difference
    );
    assert!(vault_difference > 0, "Should have claimed some fees");
}

/// Test RPC connectivity for fee claiming operations
fn test_claim_fees_rpc_connection() {
    println!("Testing RPC connection...");

    let rpc_client = RpcClient::new_with_commitment(
        DEVNET_URL,
        CommitmentConfig::confirmed(),
    );

    rpc_client.get_latest_blockhash().unwrap();
    println!("✓ RPC connection successful");
}

/// This test assumes that the validator fees vault has been initialized
#[test]
fn test_validator_claim_fees() {
    println!("Starting Validator Fee Claiming Integration Test\n");

    // 3. Fund the validator for transaction fees
    let client = RpcClient::new_with_commitment(
        DEVNET_URL,
        CommitmentConfig::confirmed(),
    );
    IntegrationTestContext::airdrop(
        &client,
        &validator_pubkey(),
        INITIAL_AIRDROP_AMOUNT,
        CommitmentConfig::confirmed(),
    )
    .expect("Failed to airdrop initial funds to validator");

    // 4. Run test sequence
    println!("=== Test 1: Instruction Creation ===");
    test_claim_fees_instruction();

    println!("\n=== Test 2: Add Test Fees ===");
    test_add_fees_to_vault();

    println!("\n=== Test 3: Claim Fees Transaction ===");
    test_claim_fees_transaction();

    println!("\n=== Test 4: RPC Connection ===");
    test_claim_fees_rpc_connection();

    println!("\nAll tests completed successfully!");
}
