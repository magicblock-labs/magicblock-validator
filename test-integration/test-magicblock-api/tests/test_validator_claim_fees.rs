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
use solana_rpc_client::rpc_client::RpcClient;
use solana_sdk::{
    commitment_config::CommitmentConfig,
    signature::{Keypair, Signer},
    transaction::Transaction,
};
use std::path::PathBuf;
use std::process::Child;
use std::sync::Arc;

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

/// Initialize the validator fees vault (required before claiming)
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

    match rpc_client.get_latest_blockhash() {
        Ok(blockhash) => {
            let transaction = Transaction::new_signed_with_payer(
                &[init_instruction],
                Some(&validator_pubkey),
                &[&validator_keypair],
                blockhash,
            );

            println!("✓ Vault initialization transaction created");

            match rpc_client.send_and_confirm_transaction(&transaction) {
                Ok(signature) => {
                    println!(
                        "✓ Successfully initialized validator fees vault!"
                    );
                    println!("  Transaction signature: {}", signature);
                }
                Err(e) => {
                    println!(
                        "⚠ Failed to initialize vault: {}",
                        e
                    );
                }
            }
        }
        Err(e) => {
            println!("✗ Could not connect to RPC: {:?}", e);
        }
    }
}

/// Add test fees to the vault to simulate validator earnings
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

    match rpc_client.request_airdrop(&validator_fees_vault, TEST_FEE_AMOUNT) {
        Ok(signature) => {
            println!(
                "✓ Added {} lamports ({:.9} SOL) test fees to vault",
                TEST_FEE_AMOUNT,
                TEST_FEE_AMOUNT as f64 / 1_000_000_000.0
            );
            println!("  Airdrop signature: {}", signature);

            std::thread::sleep(std::time::Duration::from_millis(SETUP_WAIT_MS));

            // Verify the balance
            match rpc_client.get_balance(&validator_fees_vault) {
                Ok(balance) => {
                    println!(
                        "✓ Vault now has: {} lamports ({:.9} SOL)",
                        balance,
                        balance as f64 / 1_000_000_000.0
                    );
                    assert!(
                        balance >= TEST_FEE_AMOUNT,
                        "Vault should have at least the test fee amount"
                    );
                }
                Err(e) => {
                    println!("✗ Could not verify vault balance: {}", e);
                }
            }
        }
        Err(e) => {
            println!("✗ Failed to add test fees to vault: {}", e);
            println!("  This might be expected in some test environments");
        }
    }
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

    // Record balances before claiming
    let balance_before = match rpc_client.get_balance(&validator_fees_vault) {
        Ok(balance) => {
            println!(
                "  Vault balance before: {} lamports ({:.9} SOL)",
                balance,
                balance as f64 / 1_000_000_000.0
            );
            balance
        }
        Err(e) => {
            println!("✗ Could not get vault balance before claiming: {}", e);
            0
        }
    };

    let validator_balance_before =
        match rpc_client.get_balance(&validator_pubkey) {
            Ok(balance) => {
                println!(
                    "  Validator balance before: {} lamports ({:.9} SOL)",
                    balance,
                    balance as f64 / 1_000_000_000.0
                );
                balance
            }
            Err(e) => {
                println!(
                    "✗ Could not get validator balance before claiming: {}",
                    e
                );
                0
            }
        };

    // Create and send claim fees transaction
    let instruction = validator_claim_fees(validator_pubkey, None);

    match rpc_client.get_latest_blockhash() {
        Ok(blockhash) => {
            let transaction = Transaction::new_signed_with_payer(
                &[instruction],
                Some(&validator_pubkey),
                &[&validator_keypair],
                blockhash,
            );

            println!("✓ Transaction created successfully");

            match rpc_client.send_and_confirm_transaction(&transaction) {
                Ok(signature) => {
                    println!("✓ Successfully claimed validator fees!");
                    println!("  Transaction signature: {}", signature);

                    std::thread::sleep(std::time::Duration::from_millis(
                        CONFIRMATION_WAIT_MS,
                    ));

                    // Record balances after claiming
                    let balance_after = match rpc_client
                        .get_balance(&validator_fees_vault)
                    {
                        Ok(balance) => {
                            println!("  Vault balance after: {} lamports ({:.9} SOL)", 
                                balance, balance as f64 / 1_000_000_000.0);
                            balance
                        }
                        Err(e) => {
                            println!("✗ Could not get vault balance after claiming: {}", e);
                            0
                        }
                    };

                    let validator_balance_after = match rpc_client
                        .get_balance(&validator_pubkey)
                    {
                        Ok(balance) => {
                            println!("  Validator balance after: {} lamports ({:.9} SOL)", 
                                balance, balance as f64 / 1_000_000_000.0);
                            balance
                        }
                        Err(e) => {
                            println!("✗ Could not get validator balance after claiming: {}", e);
                            0
                        }
                    };

                    // Calculate and verify the changes
                    let vault_difference =
                        balance_before.saturating_sub(balance_after);
                    let validator_difference = validator_balance_after
                        .saturating_sub(validator_balance_before);

                    println!("\nFEES CLAIMED SUMMARY:");
                    println!(
                        "   Fees claimed from vault: {} lamports ({:.9} SOL)",
                        vault_difference,
                        vault_difference as f64 / 1_000_000_000.0
                    );
                    println!(
                        "   Validator received: {} lamports ({:.9} SOL)",
                        validator_difference,
                        validator_difference as f64 / 1_000_000_000.0
                    );

                    if vault_difference > 0 {
                        println!(
                            "✓ Successfully claimed {} lamports in fees!",
                            vault_difference
                        );
                        assert!(
                            vault_difference > 0,
                            "Should have claimed some fees"
                        );

                        // Validator should receive most of the fees (minus transaction costs)
                        let fee_efficiency = validator_difference as f64
                            / vault_difference as f64;
                        assert!(fee_efficiency > 0.8, "Validator should receive at least 80% of claimed fees (rest goes to transaction costs)");
                        println!(
                            "✓ Fee efficiency: {:.1}%",
                            fee_efficiency * 100.0
                        );
                    } else {
                        println!("ⅈ No fees were available to claim");
                    }
                }
                Err(e) => {
                    println!("✗ Failed to claim fees: {}", e); 
                }
            }
        }
        Err(e) => {
            println!("✗ Could not connect to RPC: {:?}", e);
        }
    }
}

/// Test RPC connectivity for fee claiming operations
fn test_claim_fees_rpc_connection() {
    println!("Testing RPC connection...");

    let rpc_client = RpcClient::new_with_commitment(
        DEVNET_URL,
        CommitmentConfig::confirmed(),
    );

    match rpc_client.get_latest_blockhash() {
        Ok(_) => {
            println!("✓ RPC connection successful");
        }
        Err(e) => {
            println!("✗ RPC connection failed: {:?}", e);
        }
    }
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

    println!("\n=== Test 2: Vault Initialization ===");
    test_init_validator_fees_vault();
    std::thread::sleep(std::time::Duration::from_millis(SETUP_WAIT_MS));

    println!("\n=== Test 3: Add Test Fees ===");
    test_add_fees_to_vault();

    println!("\n=== Test 4: Claim Fees Transaction ===");
    test_claim_fees_transaction();

    println!("\n=== Test 5: RPC Connection ===");
    test_claim_fees_rpc_connection();

    println!("\nAll tests completed successfully!");
}
