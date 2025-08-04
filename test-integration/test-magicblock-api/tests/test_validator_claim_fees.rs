use dlp::instruction_builder::{
    init_validator_fees_vault, validator_claim_fees,
};
use integration_test_tools::validator::{
    start_test_validator_with_config, TestRunnerPaths,
};
use integration_test_tools::{
    loaded_accounts::LoadedAccounts, IntegrationTestContext,
};
use lazy_static::lazy_static;
use magicblock_program::validator;
use magicblock_validator_admin::claim_fees::ClaimFeesTask;
use solana_rpc_client::rpc_client::RpcClient;
use solana_sdk::{
    commitment_config::CommitmentConfig,
    signature::{Keypair, Signer},
    transaction::Transaction,
};
use std::path::PathBuf;
use std::process::Child;
use std::sync::Arc;
use std::thread::sleep;
use std::time::Duration;

// Test constants
const DEVNET_URL: &str = "http://127.0.0.1:7799";
const TEST_FEE_AMOUNT: u64 = 1_000_000;
const INITIAL_AIRDROP_AMOUNT: u64 = 5_000_000_000;
const CONFIRMATION_WAIT_MS: u64 = 500;
const SETUP_WAIT_MS: u64 = 1000;

lazy_static! {
    static ref VALIDATOR_KEYPAIR: Arc<Keypair> = Arc::new({
        let loaded_accounts =
            LoadedAccounts::with_delegation_program_test_authority();
        loaded_accounts
            .validator_authority_keypair()
            .insecure_clone()
    });
}

/// Test that claim fees instruction
fn test_claim_fees_instruction() {
    println!("Testing claim fees instruction creation...");

    let validator_pubkey = VALIDATOR_KEYPAIR.pubkey();
    let instruction = validator_claim_fees(validator_pubkey, None);

    assert!(
        !instruction.accounts.is_empty(),
        "Instruction should have accounts"
    );
    assert_eq!(
        instruction.program_id,
        dlp::id(),
        "Instruction should target delegation program"
    );

    println!("✓ Claim fees instruction created successfully");
}

/// Initialize the validator fees vault
fn test_init_validator_fees_vault() {
    println!("Testing validator fees vault initialization...");

    let rpc_client = RpcClient::new_with_commitment(
        DEVNET_URL,
        CommitmentConfig::confirmed(),
    );

    let validator_keypair = validator::validator_authority();
    let validator_pubkey = validator_keypair.pubkey();

    let init_instruction = init_validator_fees_vault(
        validator_pubkey,
        validator_pubkey,
        validator_pubkey,
    );

    let blockhash = rpc_client.get_latest_blockhash().unwrap();
    let transaction = Transaction::new_signed_with_payer(
        &[init_instruction],
        Some(&validator_pubkey),
        &[&validator_keypair],
        blockhash,
    );

    rpc_client
        .send_and_confirm_transaction(&transaction)
        .unwrap();
    println!("✓ Successfully initialized validator fees vault!");
}

/// Add test fees to the vault
fn test_add_fees_to_vault() {
    println!("Adding test fees to vault...");

    let rpc_client = RpcClient::new_with_commitment(
        DEVNET_URL,
        CommitmentConfig::confirmed(),
    );

    let loaded_accounts =
        LoadedAccounts::with_delegation_program_test_authority();
    let validator_fees_vault = loaded_accounts.validator_fees_vault();

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

/// Test the ClaimFeesTask struct
fn test_claim_fees_task() {
    println!("Testing ClaimFeesTask struct...");

    
    let mut task = ClaimFeesTask::new();

    // Test that the task starts in the correct state
    assert!(task.handle.is_none(), "Task should start with no handle");

    println!("✓ ClaimFeesTask created successfully");

    
    let default_task = ClaimFeesTask::default();
    assert!(
        default_task.handle.is_none(),
        "Default task should have no handle"
    );

    println!("✓ ClaimFeesTask default implementation works");
}

/// Test the actual fee claiming transaction
fn test_claim_fees_transaction() {
    println!("Testing actual claim fees transaction...");

    let rpc_client = RpcClient::new_with_commitment(
        DEVNET_URL,
        CommitmentConfig::confirmed(),
    );

    let validator_keypair = validator::validator_authority();
    let validator_pubkey = validator_keypair.pubkey();

    let loaded_accounts =
        LoadedAccounts::with_delegation_program_test_authority();
    let validator_fees_vault = loaded_accounts.validator_fees_vault();

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

struct TestValidator {
    process: Child,
}

impl TestValidator {
    fn start() -> Self {
        let manifest_dir_raw = std::env::var("CARGO_MANIFEST_DIR").unwrap();
        let manifest_dir = PathBuf::from(&manifest_dir_raw);

        let config_path = manifest_dir.join("../configs/claim-fees-test.toml");
        let workspace_dir = manifest_dir.join("../");
        let root_dir = workspace_dir.join("../");

        let paths = TestRunnerPaths {
            config_path,
            root_dir,
            workspace_dir,
        };
        let process = start_test_validator_with_config(
            &paths,
            None,
            &Default::default(),
            "CHAIN",
        )
        .expect("Failed to start devnet process");

        Self { process }
    }
}

impl Drop for TestValidator {
    fn drop(&mut self) {
        self.process
            .kill()
            .expect("Failed to stop solana-test-validator");
        self.process
            .wait()
            .expect("Failed to wait for solana-test-validator");
    }
}

fn main() {
    println!("Starting Validator Fee Claiming Integration Test\n");

    // 1. Start test infrastructure
    let _devnet = TestValidator::start();

    // 2. Initialize validator authority
    validator::init_validator_authority(
        VALIDATOR_KEYPAIR.as_ref().insecure_clone(),
    );

    // 3. Fund the validator for transaction fees
    let client = RpcClient::new_with_commitment(
        DEVNET_URL,
        CommitmentConfig::confirmed(),
    );
    IntegrationTestContext::airdrop(
        &client,
        &VALIDATOR_KEYPAIR.pubkey(),
        INITIAL_AIRDROP_AMOUNT,
        CommitmentConfig::confirmed(),
    )
    .expect("Failed to airdrop initial funds to validator");

    // 4. Run test sequence
    println!("=== Test 1: Instruction Creation ===");
    test_claim_fees_instruction();

    println!("\n=== Test 2: ClaimFeesTask Struct ===");
    test_claim_fees_task();

    println!("\n=== Test 3: Vault Initialization ===");
    test_init_validator_fees_vault();
    sleep(Duration::from_millis(SETUP_WAIT_MS));

    println!("\n=== Test 4: Add Test Fees ===");
    test_add_fees_to_vault();

    println!("\n=== Test 5: Claim Fees Transaction ===");
    test_claim_fees_transaction();

    println!("\n=== Test 6: RPC Connection ===");
    test_claim_fees_rpc_connection();

    println!("\nAll tests completed successfully!");
}
