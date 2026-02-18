use guinea::GuineaInstruction;
use magicblock_accounts_db::traits::AccountsBank;
use magicblock_magic_program_api::{
    instruction::MagicBlockInstruction, EPHEMERAL_RENT_PER_BYTE,
    EPHEMERAL_VAULT_PUBKEY, ID as MAGIC_PROGRAM_ID,
};
use solana_account::{AccountSharedData, ReadableAccount, WritableAccount};
use solana_keypair::Keypair;
use solana_program::instruction::{AccountMeta, Instruction};
use solana_pubkey::Pubkey;
use test_kit::{ExecutionTestEnv, Signer};

/// Calculates rent for an ephemeral account (same logic as magic program)
fn rent_for(data_len: u32) -> u64 {
    (u64::from(data_len) + AccountSharedData::ACCOUNT_STATIC_SIZE as u64)
        * EPHEMERAL_RENT_PER_BYTE
}

/// Test context with common setup
struct TestContext {
    env: ExecutionTestEnv,
    sponsor: Pubkey,
    ephemeral: Keypair,
}

/// Sets up a test with vault, sponsor, and ephemeral account
fn setup_test() -> TestContext {
    let env = ExecutionTestEnv::new_with_config(0, 1, false);
    init_vault(&env);

    let sponsor = env.get_payer().pubkey;
    init_sponsor(&env, sponsor);
    let ephemeral = env.create_account(0);

    TestContext {
        env,
        sponsor,
        ephemeral,
    }
}

/// Executes an instruction and returns the result
async fn execute_instruction(
    env: &ExecutionTestEnv,
    ix: Instruction,
) -> Result<(), solana_transaction_error::TransactionError> {
    let txn = env.build_transaction(&[ix]);
    env.execute_transaction(txn).await
}

/// Executes an instruction with additional signers
async fn execute_instruction_with_signers(
    env: &ExecutionTestEnv,
    ix: Instruction,
    signers: &[&Keypair],
) -> Result<(), solana_transaction_error::TransactionError> {
    let txn = env.build_transaction_with_signers(&[ix], signers);
    env.execute_transaction(txn).await
}

/// Helper to initialize the ephemeral vault account in the accounts database
fn init_vault(env: &ExecutionTestEnv) {
    // Create vault with enough balance to be rent-exempt (covers the account overhead)
    // Using a fixed amount that's sufficient for rent-exempt status
    const VAULT_RENT_EXEMPT_BALANCE: u64 = 1_000_000;
    env.fund_account_with_owner(
        EPHEMERAL_VAULT_PUBKEY,
        VAULT_RENT_EXEMPT_BALANCE,
        MAGIC_PROGRAM_ID,
    );
    let mut vault = env.get_account(EPHEMERAL_VAULT_PUBKEY);
    vault.set_ephemeral(true);
    vault.commit();
}

/// Helper to initialize the sponsor account as delegated
fn init_sponsor(env: &ExecutionTestEnv, sponsor: Pubkey) {
    // Ensure sponsor is marked as delegated so it can be modified in gasless mode
    let mut sponsor_acc = env.get_account(sponsor);
    if !sponsor_acc.delegated() {
        sponsor_acc.set_delegated(true);
        sponsor_acc.commit();
    }
}

/// Helper to create an instruction that calls guinea to create an ephemeral account
fn create_ephemeral_account_ix(
    magic_program: Pubkey,
    sponsor: Pubkey,
    ephemeral: Pubkey,
    vault: Pubkey,
    data_len: u32,
) -> Instruction {
    Instruction::new_with_bincode(
        guinea::ID,
        &GuineaInstruction::CreateEphemeralAccount { data_len },
        vec![
            AccountMeta::new_readonly(magic_program, false),
            AccountMeta::new(sponsor, true),
            AccountMeta::new(ephemeral, true),
            AccountMeta::new(vault, false),
        ],
    )
}

/// Helper to create an instruction that calls guinea to resize an ephemeral account
fn resize_ephemeral_account_ix(
    magic_program: Pubkey,
    sponsor: Pubkey,
    ephemeral: Pubkey,
    vault: Pubkey,
    new_data_len: u32,
) -> Instruction {
    Instruction::new_with_bincode(
        guinea::ID,
        &GuineaInstruction::ResizeEphemeralAccount { new_data_len },
        vec![
            AccountMeta::new_readonly(magic_program, false),
            AccountMeta::new(sponsor, true),
            AccountMeta::new(ephemeral, false),
            AccountMeta::new(vault, false),
        ],
    )
}

/// Helper to create an instruction that calls guinea to close an ephemeral account
fn close_ephemeral_account_ix(
    magic_program: Pubkey,
    sponsor: Pubkey,
    ephemeral: Pubkey,
    vault: Pubkey,
) -> Instruction {
    Instruction::new_with_bincode(
        guinea::ID,
        &GuineaInstruction::CloseEphemeralAccount,
        vec![
            AccountMeta::new_readonly(magic_program, false),
            AccountMeta::new(sponsor, true),
            AccountMeta::new(ephemeral, false),
            AccountMeta::new(vault, false),
        ],
    )
}

#[tokio::test]
async fn test_create_ephemeral_account_via_cpi() {
    let env = ExecutionTestEnv::new_with_config(0, 1, false);
    init_vault(&env);

    // Use the payer (which is a signer) as the sponsor
    let sponsor = env.get_payer().pubkey;
    init_sponsor(&env, sponsor);
    let ephemeral = env.create_account(0); // Must start with 0 lamports

    let data_len = 1000;
    let expected_rent = rent_for(data_len);

    // Check balances BEFORE operation
    let sponsor_before = env.get_account(sponsor);
    let vault_before = env.get_account(EPHEMERAL_VAULT_PUBKEY);
    let ephemeral_before = env.get_account(ephemeral.pubkey());
    let total_before = sponsor_before.lamports()
        + vault_before.lamports()
        + ephemeral_before.lamports();

    println!("=== BEFORE CREATE ===");
    println!("Sponsor: {}", sponsor_before.lamports());
    println!("Vault: {}", vault_before.lamports());
    println!("Ephemeral: {}", ephemeral_before.lamports());
    println!("Total: {}", total_before);
    println!("Expected rent: {}", expected_rent);

    let ix = create_ephemeral_account_ix(
        magicblock_magic_program_api::ID,
        sponsor,
        ephemeral.pubkey(),
        EPHEMERAL_VAULT_PUBKEY,
        data_len,
    );
    let txn = env.build_transaction_with_signers(&[ix], &[&ephemeral]);

    let result = env.execute_transaction(txn).await;
    if let Err(e) = &result {
        println!("Error executing transaction: {:?}", e);
    }
    assert!(result.is_ok());

    // Check balances AFTER operation
    let sponsor_after = env.get_account(sponsor);
    let vault_after = env.get_account(EPHEMERAL_VAULT_PUBKEY);
    let ephemeral_after = env.get_account(ephemeral.pubkey());
    let total_after = sponsor_after.lamports()
        + vault_after.lamports()
        + ephemeral_after.lamports();

    println!("=== AFTER CREATE ===");
    println!("Sponsor: {}", sponsor_after.lamports());
    println!("Vault: {}", vault_after.lamports());
    println!("Ephemeral: {}", ephemeral_after.lamports());
    println!("Total: {}", total_after);

    // Verify the ephemeral account was created correctly
    assert!(
        ephemeral_after.ephemeral(),
        "Account should be marked as ephemeral"
    );
    assert!(
        ephemeral_after.delegated(),
        "Account should be marked as delegated"
    );
    assert_eq!(
        *ephemeral_after.owner(),
        guinea::ID,
        "Owner should be guinea program"
    );
    assert_eq!(
        ephemeral_after.data().len(),
        data_len as usize,
        "Data length should match"
    );
    assert_eq!(
        ephemeral_after.lamports(),
        0,
        "Ephemeral account should have 0 lamports"
    );

    // CONSERVATION CHECK: Total lamports should not change
    assert_eq!(
        total_after, total_before,
        "Total lamports should be conserved"
    );

    // VAULT CHECK: Vault should receive exactly the rent amount
    assert_eq!(
        vault_after.lamports(),
        vault_before.lamports() + expected_rent,
        "Vault should receive exactly {} lamports",
        expected_rent
    );

    // SPONSOR CHECK: Sponsor should be charged exactly the rent amount
    assert_eq!(
        sponsor_after.lamports(),
        sponsor_before.lamports() - expected_rent,
        "Sponsor should be charged exactly {} lamports",
        expected_rent
    );
}

#[tokio::test]
async fn test_resize_ephemeral_account_via_cpi() {
    let env = ExecutionTestEnv::new_with_config(0, 1, false);
    init_vault(&env);

    // Use the payer (which is a signer) as the sponsor
    let sponsor = env.get_payer().pubkey;
    init_sponsor(&env, sponsor);
    let ephemeral = env.create_account(0);

    let initial_data_len = 1000;
    let ix = create_ephemeral_account_ix(
        magicblock_magic_program_api::ID,
        sponsor,
        ephemeral.pubkey(),
        EPHEMERAL_VAULT_PUBKEY,
        initial_data_len,
    );
    let txn = env.build_transaction_with_signers(&[ix], &[&ephemeral]);
    assert!(env.execute_transaction(txn).await.is_ok());

    // Check balances BEFORE resize
    let sponsor_before_resize = env.get_account(sponsor);
    let vault_before_resize = env.get_account(EPHEMERAL_VAULT_PUBKEY);
    let ephemeral_before_resize = env.get_account(ephemeral.pubkey());
    let total_before_resize = sponsor_before_resize.lamports()
        + vault_before_resize.lamports()
        + ephemeral_before_resize.lamports();

    println!("=== BEFORE RESIZE (GROW) ===");
    println!("Sponsor: {}", sponsor_before_resize.lamports());
    println!("Vault: {}", vault_before_resize.lamports());
    println!("Ephemeral: {}", ephemeral_before_resize.lamports());
    println!("Total: {}", total_before_resize);

    // Resize the account
    let new_data_len = 2000;
    let initial_rent = rent_for(initial_data_len);
    let new_rent = rent_for(new_data_len);
    let rent_difference = new_rent - initial_rent;

    println!("Expected rent difference: {}", rent_difference);

    let ix = resize_ephemeral_account_ix(
        magicblock_magic_program_api::ID,
        sponsor,
        ephemeral.pubkey(),
        EPHEMERAL_VAULT_PUBKEY,
        new_data_len,
    );
    let txn = env.build_transaction(&[ix]);
    assert!(env.execute_transaction(txn).await.is_ok());

    // Check balances AFTER resize
    let sponsor_after_resize = env.get_account(sponsor);
    let vault_after_resize = env.get_account(EPHEMERAL_VAULT_PUBKEY);
    let ephemeral_after_resize = env.get_account(ephemeral.pubkey());
    let total_after_resize = sponsor_after_resize.lamports()
        + vault_after_resize.lamports()
        + ephemeral_after_resize.lamports();

    println!("=== AFTER RESIZE (GROW) ===");
    println!("Sponsor: {}", sponsor_after_resize.lamports());
    println!("Vault: {}", vault_after_resize.lamports());
    println!("Ephemeral: {}", ephemeral_after_resize.lamports());
    println!("Total: {}", total_after_resize);

    // Verify the account was resized
    assert_eq!(
        ephemeral_after_resize.data().len(),
        new_data_len as usize,
        "Data length should be updated"
    );
    assert!(
        ephemeral_after_resize.ephemeral(),
        "Account should still be ephemeral"
    );

    // CONSERVATION CHECK: Total lamports should not change
    assert_eq!(
        total_after_resize, total_before_resize,
        "Total lamports should be conserved"
    );

    // VAULT CHECK: Vault should receive exactly the rent difference
    assert_eq!(
        vault_after_resize.lamports(),
        vault_before_resize.lamports() + rent_difference,
        "Vault should receive exactly {} lamports",
        rent_difference
    );

    // SPONSOR CHECK: Sponsor should be charged exactly the rent difference
    assert_eq!(
        sponsor_after_resize.lamports(),
        sponsor_before_resize.lamports() - rent_difference,
        "Sponsor should be charged exactly {} lamports",
        rent_difference
    );
}

#[tokio::test]
async fn test_close_ephemeral_account_via_cpi() {
    let env = ExecutionTestEnv::new_with_config(0, 1, false);
    init_vault(&env);

    // Use the payer (which is a signer) as the sponsor
    let sponsor = env.get_payer().pubkey;
    init_sponsor(&env, sponsor);
    let ephemeral = env.create_account(0);

    let data_len = 1000;
    let ix = create_ephemeral_account_ix(
        magicblock_magic_program_api::ID,
        sponsor,
        ephemeral.pubkey(),
        EPHEMERAL_VAULT_PUBKEY,
        data_len,
    );
    let txn = env.build_transaction_with_signers(&[ix], &[&ephemeral]);
    assert!(env.execute_transaction(txn).await.is_ok());

    let expected_rent = rent_for(data_len);

    // Check balances BEFORE close
    let sponsor_before_close = env.get_account(sponsor);
    let vault_before_close = env.get_account(EPHEMERAL_VAULT_PUBKEY);
    let ephemeral_before_close = env.get_account(ephemeral.pubkey());
    let total_before_close = sponsor_before_close.lamports()
        + vault_before_close.lamports()
        + ephemeral_before_close.lamports();

    println!("=== BEFORE CLOSE ===");
    println!("Sponsor: {}", sponsor_before_close.lamports());
    println!("Vault: {}", vault_before_close.lamports());
    println!("Ephemeral: {}", ephemeral_before_close.lamports());
    println!("Total: {}", total_before_close);
    println!("Expected refund: {}", expected_rent);

    // Close the ephemeral account
    let ix = close_ephemeral_account_ix(
        magicblock_magic_program_api::ID,
        sponsor,
        ephemeral.pubkey(),
        EPHEMERAL_VAULT_PUBKEY,
    );
    let txn = env.build_transaction(&[ix]);

    let result = env.execute_transaction(txn).await;
    if let Err(e) = &result {
        println!("Error closing account: {:?}", e);
    }
    assert!(result.is_ok());

    // Check balances AFTER close
    let sponsor_after_close = env.get_account(sponsor);
    let vault_after_close = env.get_account(EPHEMERAL_VAULT_PUBKEY);

    // Closed ephemeral accounts are removed from the DB
    assert!(
        env.try_get_account(ephemeral.pubkey()).is_none(),
        "Closed ephemeral account should be removed from DB"
    );

    let total_after_close =
        sponsor_after_close.lamports() + vault_after_close.lamports();

    println!("=== AFTER CLOSE ===");
    println!("Sponsor: {}", sponsor_after_close.lamports());
    println!("Vault: {}", vault_after_close.lamports());
    println!("Total: {}", total_after_close);

    // CONSERVATION CHECK: Total lamports should not change
    // (ephemeral had 0 lamports, so sponsor + vault should equal prior total)
    assert_eq!(
        total_after_close, total_before_close,
        "Total lamports should be conserved"
    );

    // VAULT CHECK: Vault should pay out exactly the expected rent
    assert_eq!(
        vault_after_close.lamports(),
        vault_before_close.lamports() - expected_rent,
        "Vault should pay out exactly {} lamports",
        expected_rent
    );

    // SPONSOR CHECK: Sponsor should receive exactly the expected refund
    assert_eq!(
        sponsor_after_close.lamports(),
        sponsor_before_close.lamports() + expected_rent,
        "Sponsor should receive exactly {} lamports refund",
        expected_rent
    );
}

#[tokio::test]
async fn test_resize_smaller_via_cpi() {
    let env = ExecutionTestEnv::new_with_config(0, 1, false);
    init_vault(&env);

    // Use the payer (which is a signer) as the sponsor
    let sponsor = env.get_payer().pubkey;
    init_sponsor(&env, sponsor);
    let ephemeral = env.create_account(0);

    let initial_data_len = 2000;
    let ix = create_ephemeral_account_ix(
        magicblock_magic_program_api::ID,
        sponsor,
        ephemeral.pubkey(),
        EPHEMERAL_VAULT_PUBKEY,
        initial_data_len,
    );
    let txn = env.build_transaction_with_signers(&[ix], &[&ephemeral]);
    assert!(env.execute_transaction(txn).await.is_ok());

    // Check balances BEFORE resize (shrink)
    let sponsor_before_resize = env.get_account(sponsor);
    let vault_before_resize = env.get_account(EPHEMERAL_VAULT_PUBKEY);
    let ephemeral_before_resize = env.get_account(ephemeral.pubkey());
    let total_before_resize = sponsor_before_resize.lamports()
        + vault_before_resize.lamports()
        + ephemeral_before_resize.lamports();

    println!("=== BEFORE RESIZE (SHRINK) ===");
    println!("Sponsor: {}", sponsor_before_resize.lamports());
    println!("Vault: {}", vault_before_resize.lamports());
    println!("Ephemeral: {}", ephemeral_before_resize.lamports());
    println!("Total: {}", total_before_resize);

    // Resize to smaller
    let new_data_len = 1000;
    let initial_rent = rent_for(initial_data_len);
    let new_rent = rent_for(new_data_len);
    let rent_refund = initial_rent - new_rent;

    println!("Expected refund: {}", rent_refund);

    let ix = resize_ephemeral_account_ix(
        magicblock_magic_program_api::ID,
        sponsor,
        ephemeral.pubkey(),
        EPHEMERAL_VAULT_PUBKEY,
        new_data_len,
    );
    let txn = env.build_transaction(&[ix]);
    assert!(env.execute_transaction(txn).await.is_ok());

    // Check balances AFTER resize
    let sponsor_after_resize = env.get_account(sponsor);
    let vault_after_resize = env.get_account(EPHEMERAL_VAULT_PUBKEY);
    let ephemeral_after_resize = env.get_account(ephemeral.pubkey());
    let total_after_resize = sponsor_after_resize.lamports()
        + vault_after_resize.lamports()
        + ephemeral_after_resize.lamports();

    println!("=== AFTER RESIZE (SHRINK) ===");
    println!("Sponsor: {}", sponsor_after_resize.lamports());
    println!("Vault: {}", vault_after_resize.lamports());
    println!("Ephemeral: {}", ephemeral_after_resize.lamports());
    println!("Total: {}", total_after_resize);

    // Verify the account was resized
    assert_eq!(
        ephemeral_after_resize.data().len(),
        new_data_len as usize,
        "Data length should be updated"
    );

    // CONSERVATION CHECK: Total lamports should not change
    assert_eq!(
        total_after_resize, total_before_resize,
        "Total lamports should be conserved"
    );

    // VAULT CHECK: Vault should pay out exactly the refund
    assert_eq!(
        vault_after_resize.lamports(),
        vault_before_resize.lamports() - rent_refund,
        "Vault should pay out exactly {} lamports",
        rent_refund
    );

    // SPONSOR CHECK: Sponsor should receive exactly the refund
    assert_eq!(
        sponsor_after_resize.lamports(),
        sponsor_before_resize.lamports() + rent_refund,
        "Sponsor should receive exactly {} lamports refund",
        rent_refund
    );
}

// ============================================================================
// Failure & Validation Tests
// ============================================================================

/// Helper to create a direct magic-program instruction (not via CPI)
fn direct_create_instruction(
    sponsor: Pubkey,
    ephemeral: Pubkey,
    vault: Pubkey,
    data_len: u32,
) -> Instruction {
    Instruction::new_with_bincode(
        magicblock_magic_program_api::ID,
        &MagicBlockInstruction::CreateEphemeralAccount { data_len },
        vec![
            AccountMeta::new(sponsor, true),
            AccountMeta::new(ephemeral, false),
            AccountMeta::new(vault, false),
        ],
    )
}

#[tokio::test]
async fn test_direct_call_rejected() {
    let env = ExecutionTestEnv::new_with_config(0, 1, false);
    init_vault(&env);

    let sponsor = env.get_payer().pubkey;
    init_sponsor(&env, sponsor);
    let ephemeral = env.create_account(0);

    // Call magic-program directly (NOT via CPI) - should fail
    let ix = direct_create_instruction(
        sponsor,
        ephemeral.pubkey(),
        EPHEMERAL_VAULT_PUBKEY,
        1000,
    );
    let txn = env.build_transaction(&[ix]);

    let result = env.execute_transaction(txn).await;
    assert!(result.is_err(), "Direct call should be rejected");
}

#[tokio::test]
async fn test_create_with_non_zero_lamports_fails() {
    let env = ExecutionTestEnv::new_with_config(0, 1, false);
    init_vault(&env);

    let sponsor = env.get_payer().pubkey;
    init_sponsor(&env, sponsor);
    let ephemeral = env.create_account(100); // Non-zero lamports!

    let result = execute_instruction_with_signers(
        &env,
        create_ephemeral_account_ix(
            magicblock_magic_program_api::ID,
            sponsor,
            ephemeral.pubkey(),
            EPHEMERAL_VAULT_PUBKEY,
            1000,
        ),
        &[&ephemeral],
    )
    .await;

    assert!(result.is_err(), "Should fail with non-zero lamports");
}

#[tokio::test]
async fn test_create_already_ephemeral_fails() {
    let ctx = setup_test();

    // Simulate an existing ephemeral account (owned by a program, not system)
    let mut acc = ctx.env.get_account(ctx.ephemeral.pubkey());
    acc.set_owner(guinea::ID);
    acc.set_ephemeral(true);
    acc.commit();

    let result = execute_instruction_with_signers(
        &ctx.env,
        create_ephemeral_account_ix(
            magicblock_magic_program_api::ID,
            ctx.sponsor,
            ctx.ephemeral.pubkey(),
            EPHEMERAL_VAULT_PUBKEY,
            1000,
        ),
        &[&ctx.ephemeral],
    )
    .await;

    assert!(result.is_err(), "Should fail - already ephemeral");
}

#[tokio::test]
async fn test_create_with_wrong_vault_fails() {
    let env = ExecutionTestEnv::new_with_config(0, 1, false);
    init_vault(&env);

    let sponsor = env.get_payer().pubkey;
    init_sponsor(&env, sponsor);
    let ephemeral = env.create_account(0).pubkey();

    // Use wrong vault (not owned by magic-program)
    let wrong_vault = env.create_account(1000).pubkey();

    let ix = Instruction::new_with_bincode(
        guinea::ID,
        &GuineaInstruction::CreateEphemeralAccount { data_len: 1000 },
        vec![
            AccountMeta::new_readonly(magicblock_magic_program_api::ID, false),
            AccountMeta::new(sponsor, true),
            AccountMeta::new(ephemeral, false),
            AccountMeta::new(wrong_vault, false),
        ],
    );

    let result = execute_instruction(&env, ix).await;
    assert!(result.is_err(), "Should fail with wrong vault");
}

#[tokio::test]
async fn test_resize_non_ephemeral_fails() {
    let ctx = setup_test();
    let regular = ctx.env.create_account(0).pubkey();

    // Try to resize a non-ephemeral account
    let result = execute_instruction(
        &ctx.env,
        resize_ephemeral_account_ix(
            magicblock_magic_program_api::ID,
            ctx.sponsor,
            regular,
            EPHEMERAL_VAULT_PUBKEY,
            1000,
        ),
    )
    .await;

    assert!(result.is_err(), "Should fail - not ephemeral");
}

#[tokio::test]
async fn test_close_non_ephemeral_fails() {
    let ctx = setup_test();
    let regular = ctx.env.create_account(0).pubkey();

    // Try to close a non-ephemeral account
    let result = execute_instruction(
        &ctx.env,
        close_ephemeral_account_ix(
            magicblock_magic_program_api::ID,
            ctx.sponsor,
            regular,
            EPHEMERAL_VAULT_PUBKEY,
        ),
    )
    .await;

    assert!(result.is_err(), "Should fail - not ephemeral");
}

#[tokio::test]
async fn test_resize_to_zero_size() {
    let env = ExecutionTestEnv::new_with_config(0, 1, false);
    init_vault(&env);

    let sponsor = env.get_payer().pubkey;
    init_sponsor(&env, sponsor);
    let ephemeral = env.create_account(0);

    // Create with initial size
    let ix = create_ephemeral_account_ix(
        magicblock_magic_program_api::ID,
        sponsor,
        ephemeral.pubkey(),
        EPHEMERAL_VAULT_PUBKEY,
        1000,
    );
    let txn = env.build_transaction_with_signers(&[ix], &[&ephemeral]);
    assert!(env.execute_transaction(txn).await.is_ok());

    // Resize to zero
    let ix = resize_ephemeral_account_ix(
        magicblock_magic_program_api::ID,
        sponsor,
        ephemeral.pubkey(),
        EPHEMERAL_VAULT_PUBKEY,
        0,
    );
    let txn = env.build_transaction(&[ix]);

    let result = env.execute_transaction(txn).await;
    assert!(result.is_ok(), "Should allow resize to zero");

    let ephemeral_after = env.get_account(ephemeral.pubkey());
    assert_eq!(ephemeral_after.data().len(), 0);
}

#[tokio::test]
async fn test_close_already_closed() {
    let env = ExecutionTestEnv::new_with_config(0, 1, false);
    init_vault(&env);

    let sponsor = env.get_payer().pubkey;
    init_sponsor(&env, sponsor);
    let ephemeral = env.create_account(0);

    // Create and close
    let create_ix = create_ephemeral_account_ix(
        magicblock_magic_program_api::ID,
        sponsor,
        ephemeral.pubkey(),
        EPHEMERAL_VAULT_PUBKEY,
        1000,
    );
    let txn = env.build_transaction_with_signers(&[create_ix], &[&ephemeral]);
    assert!(env.execute_transaction(txn).await.is_ok());

    let close_ix = close_ephemeral_account_ix(
        magicblock_magic_program_api::ID,
        sponsor,
        ephemeral.pubkey(),
        EPHEMERAL_VAULT_PUBKEY,
    );
    let txn = env.build_transaction(&[close_ix]);
    assert!(env.execute_transaction(txn).await.is_ok());

    // Try to close again - should fail because account is now owned by system-program
    let close_ix2 = close_ephemeral_account_ix(
        magicblock_magic_program_api::ID,
        sponsor,
        ephemeral.pubkey(),
        EPHEMERAL_VAULT_PUBKEY,
    );
    let txn = env.build_transaction(&[close_ix2]);

    let result = env.execute_transaction(txn).await;
    assert!(
        result.is_err(),
        "Re-close should fail (account owned by system-program)"
    );
}

#[tokio::test]
async fn test_close_already_closed_double_spend() {
    let env = ExecutionTestEnv::new_with_config(0, 1, false);
    init_vault(&env);

    let sponsor = env.get_payer().pubkey;
    init_sponsor(&env, sponsor);
    let ephemeral = env.create_account(0);

    // Create and close
    let ix = create_ephemeral_account_ix(
        magicblock_magic_program_api::ID,
        sponsor,
        ephemeral.pubkey(),
        EPHEMERAL_VAULT_PUBKEY,
        1000,
    );
    let txn = env.build_transaction_with_signers(&[ix], &[&ephemeral]);
    assert!(env.execute_transaction(txn).await.is_ok());

    // Track balances before first close
    let sponsor_before =
        env.accountsdb.get_account(&sponsor).unwrap().lamports();
    let vault_before = env
        .accountsdb
        .get_account(&EPHEMERAL_VAULT_PUBKEY)
        .unwrap()
        .lamports();

    let close_ix = close_ephemeral_account_ix(
        magicblock_magic_program_api::ID,
        sponsor,
        ephemeral.pubkey(),
        EPHEMERAL_VAULT_PUBKEY,
    );
    let txn = env.build_transaction(&[close_ix]);
    assert!(env.execute_transaction(txn).await.is_ok());

    let sponsor_after_close =
        env.accountsdb.get_account(&sponsor).unwrap().lamports();
    let vault_after_close = env
        .accountsdb
        .get_account(&EPHEMERAL_VAULT_PUBKEY)
        .unwrap()
        .lamports();

    let first_refund = sponsor_after_close - sponsor_before;
    let first_vault_debit = vault_before - vault_after_close;

    println!(
        "First close - Sponsor refund: {}, Vault debit: {}",
        first_refund, first_vault_debit
    );

    let close_ix2 = close_ephemeral_account_ix(
        magicblock_magic_program_api::ID,
        sponsor,
        ephemeral.pubkey(),
        EPHEMERAL_VAULT_PUBKEY,
    );
    let txn = env.build_transaction(&[close_ix2]);
    let result = env.execute_transaction(txn).await;

    assert!(
        result.is_err(),
        "Re-close should fail (account not owned by magic-program)"
    );

    // Verify no additional refund was given
    let sponsor_after_close2 =
        env.accountsdb.get_account(&sponsor).unwrap().lamports();
    let vault_after_close2 = env
        .accountsdb
        .get_account(&EPHEMERAL_VAULT_PUBKEY)
        .unwrap()
        .lamports();

    assert_eq!(
        sponsor_after_close2, sponsor_after_close,
        "Sponsor should not get second refund"
    );
    assert_eq!(
        vault_after_close2, vault_after_close,
        "Vault should not be debited twice"
    );
}

#[tokio::test]
async fn test_insufficient_balance_fails() {
    let env = ExecutionTestEnv::new_with_config(0, 1, false);
    init_vault(&env);

    // Use the payer but give it very low balance
    let sponsor = env.get_payer().pubkey;

    // Set sponsor balance to very low amount
    let mut sponsor_acc = env.get_account(sponsor);
    sponsor_acc.set_lamports(100); // Only 100 lamports
    sponsor_acc.commit();

    let ephemeral = env.create_account(0);

    let data_len = 1000;
    let required_rent = rent_for(data_len);

    assert!(required_rent > 100, "Rent should exceed sponsor balance");

    let ix = create_ephemeral_account_ix(
        magicblock_magic_program_api::ID,
        sponsor,
        ephemeral.pubkey(),
        EPHEMERAL_VAULT_PUBKEY,
        data_len,
    );
    let txn = env.build_transaction_with_signers(&[ix], &[&ephemeral]);

    let result = env.execute_transaction(txn).await;
    assert!(result.is_err(), "Should fail - insufficient balance");
}

// Tests creating an ephemeral account with a PDA sponsor.
// The guinea program uses `invoke_signed` with proper seeds to sign for the PDA.
#[tokio::test]
async fn test_create_with_pda_sponsor() {
    let env = ExecutionTestEnv::new_with_config(0, 1, false);
    init_vault(&env);

    // 1. Derive the global sponsor PDA (same seed as in guinea program)
    let (global_sponsor_pda, _bump) =
        Pubkey::find_program_address(&[b"global_sponsor"], &guinea::ID);

    // 2. Insert into accountsdb with 1 SOL and 32 bytes data, owned by guinea
    let mut account = AccountSharedData::new(1_000_000_000, 32, &guinea::ID);
    account.set_delegated(true);
    let _ = env.accountsdb.insert_account(&global_sponsor_pda, &account);

    let ephemeral = env.create_account(0);

    // 3. Try creating ephemeral account with PDA sponsor using the regular instruction
    // guinea will detect the PDA and patch the instruction to use invoke_signed
    let ix = Instruction::new_with_bincode(
        guinea::ID,
        &GuineaInstruction::CreateEphemeralAccount { data_len: 1000 },
        vec![
            AccountMeta::new_readonly(magicblock_magic_program_api::ID, false),
            AccountMeta::new(global_sponsor_pda, false), // PDA (not a signer in transaction)
            AccountMeta::new(ephemeral.pubkey(), true),  // Ephemeral must sign
            AccountMeta::new(EPHEMERAL_VAULT_PUBKEY, false),
        ],
    );
    let txn = env.build_transaction_with_signers(&[ix], &[&ephemeral]);

    let result = env.execute_transaction(txn).await;

    // Should succeed - guinea patches the instruction and uses invoke_signed
    match result {
        Ok(_) => {
            // Verify ephemeral account was created successfully
            let ephemeral_acc = env.get_account(ephemeral.pubkey());
            assert!(ephemeral_acc.ephemeral(), "Account should be ephemeral");
            assert_eq!(
                ephemeral_acc.data().len(),
                1000,
                "Account should have 1000 bytes"
            );
        }
        Err(e) => {
            panic!("PDA sponsor test failed with: {:?}", e);
        }
    }
}

#[tokio::test]
async fn test_pda_wrong_owner_fails() {
    let env = ExecutionTestEnv::new_with_config(0, 1, false);
    init_vault(&env);

    // Create a PDA owned by system program (not guinea)
    let (pda, _bump) = Pubkey::find_program_address(
        &[b"wrong"],
        &solana_sdk_ids::system_program::id(),
    );
    let mut account =
        AccountSharedData::new(0, 0, &solana_sdk_ids::system_program::id());
    account.set_delegated(true);
    let _ = env.accountsdb.insert_account(&pda, &account);

    let ephemeral = env.create_account(0);

    let ix = Instruction::new_with_bincode(
        guinea::ID,
        &GuineaInstruction::CreateEphemeralAccount { data_len: 1000 },
        vec![
            AccountMeta::new_readonly(magicblock_magic_program_api::ID, false),
            AccountMeta::new(pda, false), // NOT a signer, not owned by guinea
            AccountMeta::new(ephemeral.pubkey(), false),
            AccountMeta::new(EPHEMERAL_VAULT_PUBKEY, false),
        ],
    );
    let txn = env.build_transaction(&[ix]);

    let result = env.execute_transaction(txn).await;
    assert!(result.is_err(), "Should fail - PDA not owned by caller");
}

#[tokio::test]
async fn test_non_signer_oncurve_sponsor_fails() {
    let env = ExecutionTestEnv::new_with_config(0, 1, false);
    init_vault(&env);

    // Create oncurve account that is NOT a signer
    let non_signer = env.create_account(1000);
    let ephemeral = env.create_account(0);

    let ix = Instruction::new_with_bincode(
        guinea::ID,
        &GuineaInstruction::CreateEphemeralAccount { data_len: 1000 },
        vec![
            AccountMeta::new_readonly(magicblock_magic_program_api::ID, false),
            AccountMeta::new(non_signer.pubkey(), false), // NOT a signer
            AccountMeta::new(ephemeral.pubkey(), false),
            AccountMeta::new(EPHEMERAL_VAULT_PUBKEY, false),
        ],
    );
    let txn = env.build_transaction(&[ix]);

    let result = env.execute_transaction(txn).await;
    assert!(result.is_err(), "Should fail - non-signer oncurve account");
}

#[tokio::test]
async fn test_full_lifecycle() {
    let env = ExecutionTestEnv::new_with_config(0, 1, false);
    init_vault(&env);

    let sponsor = env.get_payer().pubkey;
    init_sponsor(&env, sponsor);
    let ephemeral = env.create_account(0);

    // Create → Resize (grow) → Resize (shrink) → Close
    let create_ix = create_ephemeral_account_ix(
        magicblock_magic_program_api::ID,
        sponsor,
        ephemeral.pubkey(),
        EPHEMERAL_VAULT_PUBKEY,
        1000,
    );
    let txn = env.build_transaction_with_signers(&[create_ix], &[&ephemeral]);
    assert!(env.execute_transaction(txn).await.is_ok());

    let grow_ix = resize_ephemeral_account_ix(
        magicblock_magic_program_api::ID,
        sponsor,
        ephemeral.pubkey(),
        EPHEMERAL_VAULT_PUBKEY,
        2000,
    );
    let txn = env.build_transaction(&[grow_ix]);
    assert!(env.execute_transaction(txn).await.is_ok());

    let shrink_ix = resize_ephemeral_account_ix(
        magicblock_magic_program_api::ID,
        sponsor,
        ephemeral.pubkey(),
        EPHEMERAL_VAULT_PUBKEY,
        500,
    );
    let txn = env.build_transaction(&[shrink_ix]);
    assert!(env.execute_transaction(txn).await.is_ok());

    let close_ix = close_ephemeral_account_ix(
        magicblock_magic_program_api::ID,
        sponsor,
        ephemeral.pubkey(),
        EPHEMERAL_VAULT_PUBKEY,
    );
    let txn = env.build_transaction(&[close_ix]);
    assert!(env.execute_transaction(txn).await.is_ok());

    // Closed ephemeral accounts are removed from the DB
    assert!(
        env.try_get_account(ephemeral.pubkey()).is_none(),
        "Closed ephemeral account should be removed from DB"
    );
}

#[tokio::test]
async fn test_multiple_accounts_same_sponsor() {
    let env = ExecutionTestEnv::new_with_config(0, 1, false);
    init_vault(&env);

    // Use payer[0] as sponsor - need to be explicit about which payer
    let sponsor = env.payers[0].pubkey();
    init_sponsor(&env, sponsor);

    let eph1 = env.create_account(0);
    let eph2 = env.create_account(0);
    let eph3 = env.create_account(0);

    let total_rent = rent_for(1000) + rent_for(2000) + rent_for(500);

    // Get initial balance directly from AccountsDB to avoid caching issues
    let sponsor_balance_before =
        env.accountsdb.get_account(&sponsor).unwrap().lamports();

    // Create 3 accounts with different sizes
    for (eph, len) in [(&eph1, 1000), (&eph2, 2000), (&eph3, 500)] {
        let ix = create_ephemeral_account_ix(
            magicblock_magic_program_api::ID,
            sponsor,
            eph.pubkey(),
            EPHEMERAL_VAULT_PUBKEY,
            len,
        );
        let txn = env.build_transaction_with_signers(&[ix], &[eph]);
        assert!(env.execute_transaction(txn).await.is_ok());
    }

    // Get final balance directly from AccountsDB
    let sponsor_balance_after =
        env.accountsdb.get_account(&sponsor).unwrap().lamports();

    assert_eq!(
        sponsor_balance_before - sponsor_balance_after,
        total_rent,
        "Sponsor should be charged total rent for all accounts"
    );
}
