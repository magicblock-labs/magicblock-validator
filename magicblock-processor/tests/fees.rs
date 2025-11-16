use std::{collections::HashSet, time::Duration};

use guinea::GuineaInstruction;
use magicblock_core::traits::AccountsBank;
use solana_account::{ReadableAccount, WritableAccount};
use solana_keypair::Keypair;
use solana_program::{
    instruction::{AccountMeta, Instruction},
    native_token::LAMPORTS_PER_SOL,
};
use solana_pubkey::Pubkey;
use solana_transaction_error::TransactionError;
use test_kit::{ExecutionTestEnv, Signer};

pub const DELEGATION_PROGRAM_ID: Pubkey =
    Pubkey::from_str_const("DELeGGvXpWV2fqJUhqcF5ZSYMS4JTLjteaAMARRSaeSh");

/// A helper to derive the ephemeral balance PDA for a given payer.
/// This logic is specific to the delegation program being tested.
pub fn ephemeral_balance_pda_from_payer(payer: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &[b"balance", payer.as_ref(), &[0]],
        &DELEGATION_PROGRAM_ID,
    )
    .0
}

/// A test helper to build a simple instruction targeting the `guinea` test program.
fn setup_guinea_instruction(
    env: &ExecutionTestEnv,
    ix_data: &GuineaInstruction,
    is_writable: bool,
) -> (Instruction, Pubkey) {
    let account = env
        .create_account_with_config(LAMPORTS_PER_SOL, 128, guinea::ID)
        .pubkey();
    let meta = if is_writable {
        AccountMeta::new(account, false)
    } else {
        AccountMeta::new_readonly(account, false)
    };
    let ix = Instruction::new_with_bincode(guinea::ID, ix_data, vec![meta]);
    (ix, account)
}

/// Verifies that a transaction fails if the fee payer has insufficient lamports.
#[tokio::test]
async fn test_insufficient_fee() {
    let env = ExecutionTestEnv::new();
    let mut payer = env.get_payer();
    payer.set_lamports(ExecutionTestEnv::BASE_FEE - 1);
    payer.commmit();

    let (ix, _) =
        setup_guinea_instruction(&env, &GuineaInstruction::PrintSizes, false);
    let txn = env.build_transaction(&[ix]);

    let result = env.execute_transaction(txn).await;
    assert!(matches!(
        result,
        Err(TransactionError::InsufficientFundsForFee)
    ));
}

/// Verifies a transaction succeeds with a fee payer distinct from instruction accounts.
#[tokio::test]
async fn test_separate_fee_payer() {
    let env = ExecutionTestEnv::new();
    let sender =
        env.create_account_with_config(LAMPORTS_PER_SOL, 0, guinea::ID);
    let recipient = env.create_account(LAMPORTS_PER_SOL);
    let fee_payer_initial_balance = env.get_payer().lamports();
    const TRANSFER_AMOUNT: u64 = 1_000_000;

    let ix = Instruction::new_with_bincode(
        guinea::ID,
        &GuineaInstruction::Transfer(TRANSFER_AMOUNT),
        vec![
            AccountMeta::new(sender.pubkey(), false),
            AccountMeta::new(recipient.pubkey(), false),
        ],
    );
    let txn = env.build_transaction(&[ix]);

    env.execute_transaction(txn).await.unwrap();

    let sender_final = env.get_account(sender.pubkey()).lamports();
    let recipient_final = env.get_account(recipient.pubkey()).lamports();
    let fee_payer_final = env.get_payer().lamports();

    assert_eq!(sender_final, LAMPORTS_PER_SOL - TRANSFER_AMOUNT);
    assert_eq!(recipient_final, LAMPORTS_PER_SOL + TRANSFER_AMOUNT);
    assert_eq!(
        fee_payer_final,
        fee_payer_initial_balance - ExecutionTestEnv::BASE_FEE
    );
}

/// Verifies a transaction is rejected if its fee payer is not a delegated account.
#[tokio::test]
async fn test_non_delegated_payer_rejection() {
    let env = ExecutionTestEnv::new();
    let mut payer = env.get_payer();
    payer.set_delegated(false); // Mark the payer as not delegated
    let fee_payer_initial_balance = payer.lamports();
    payer.commmit();

    let (ix, _) =
        setup_guinea_instruction(&env, &GuineaInstruction::PrintSizes, false);
    let txn = env.build_transaction(&[ix]);

    let result = env.execute_transaction(txn).await;
    assert!(
        matches!(result, Err(TransactionError::InvalidAccountForFee)),
        "transaction should be rejected if payer is not delegated"
    );

    let fee_payer_final_balance = env.get_payer().lamports();
    assert_eq!(
        fee_payer_final_balance, fee_payer_initial_balance,
        "payer should not be charged a fee for a rejected transaction"
    );
}

/// Verifies that a transaction can use a delegated escrow account to pay fees
/// when the primary fee payer is not delegated.
#[tokio::test]
async fn test_escrowed_payer_success() {
    let env = ExecutionTestEnv::new();
    let mut payer = env.get_payer();
    payer.set_lamports(ExecutionTestEnv::BASE_FEE - 1);
    payer.set_delegated(false);
    let escrow = ephemeral_balance_pda_from_payer(&payer.pubkey);
    payer.commmit();

    env.fund_account(escrow, LAMPORTS_PER_SOL); // Fund the escrow PDA

    let fee_payer_initial_balance = env.get_payer().lamports();
    let escrow_initial_balance = env.get_account(escrow).lamports();
    const ACCOUNT_SIZE: usize = 1024;

    let (ix, account_to_resize) = setup_guinea_instruction(
        &env,
        &GuineaInstruction::Resize(ACCOUNT_SIZE),
        true,
    );
    let txn = env.build_transaction(&[ix]);

    env.execute_transaction(txn)
        .await
        .expect("escrow swap transaction should succeed");

    let fee_payer_final_balance = env.get_payer().lamports();
    let escrow_final_balance = env.get_account(escrow).lamports();
    let final_account_size = env.get_account(account_to_resize).data().len();
    let mut updated_accounts = HashSet::new();
    while let Ok(acc) = env.dispatch.account_update.try_recv() {
        updated_accounts.insert(acc.account.pubkey);
    }

    println!("escrow: {escrow}\naccounts: {updated_accounts:?}");
    assert_eq!(
        fee_payer_final_balance, fee_payer_initial_balance,
        "primary payer should not be charged"
    );
    assert_eq!(
        escrow_final_balance,
        escrow_initial_balance - ExecutionTestEnv::BASE_FEE,
        "escrow account should have paid the fee"
    );
    assert!(
        updated_accounts.contains(&escrow),
        "escrow account update should have been sent"
    );
    assert!(
        !updated_accounts.contains(&env.payer.pubkey()),
        "orginal payer account update should not have been sent"
    );
    assert_eq!(
        final_account_size, ACCOUNT_SIZE,
        "instruction side effects should be committed on success"
    );
}

/// Verifies the fee payer is charged even when the transaction fails during execution.
#[tokio::test]
async fn test_fee_charged_for_failed_transaction() {
    let env = ExecutionTestEnv::new();
    let fee_payer_initial_balance = env.get_payer().lamports();
    let account = env
        .create_account_with_config(LAMPORTS_PER_SOL, 0, guinea::ID) // Account with no data
        .pubkey();

    // This instruction will fail because it tries to write to an account with 0 data length.
    let ix = Instruction::new_with_bincode(
        guinea::ID,
        &GuineaInstruction::WriteByteToData(42),
        vec![AccountMeta::new(account, false)],
    );
    let txn = env.build_transaction(&[ix]);

    // `schedule` is used to bypass preflight checks that might catch the error early.
    env.transaction_scheduler.schedule(txn).await.unwrap();

    let status = env
        .dispatch
        .transaction_status
        .recv_timeout(Duration::from_millis(100))
        .expect("no transaction status received for failed txn");

    assert!(
        status.result.result.is_err(),
        "transaction should have failed"
    );
    let fee_payer_final_balance = env.get_payer().lamports();
    assert_eq!(
        fee_payer_final_balance,
        fee_payer_initial_balance - ExecutionTestEnv::BASE_FEE,
        "payer should be charged a fee even for a failed transaction"
    );
}

/// Verifies the fee is charged to the escrow account for a failed transaction.
#[tokio::test]
async fn test_escrow_charged_for_failed_transaction() {
    let env = ExecutionTestEnv::new();
    let mut payer = env.get_payer();
    payer.set_lamports(0);
    payer.set_delegated(false);
    let escrow = ephemeral_balance_pda_from_payer(&payer.pubkey);
    payer.commmit();
    let account = env
        .create_account_with_config(LAMPORTS_PER_SOL, 0, guinea::ID) // Account with no data
        .pubkey();

    env.fund_account(escrow, LAMPORTS_PER_SOL);
    let escrow_initial_balance = env.get_account(escrow).lamports();

    // This instruction will fail because it tries to write to an account with 0 data length.
    let ix = Instruction::new_with_bincode(
        guinea::ID,
        &GuineaInstruction::WriteByteToData(42),
        vec![AccountMeta::new(account, false)],
    );
    let txn = env.build_transaction(&[ix]);

    env.transaction_scheduler.schedule(txn).await.unwrap();

    let status = env
        .dispatch
        .transaction_status
        .recv_timeout(Duration::from_millis(100))
        .expect("no transaction status received for failed escrow txn");

    assert!(
        status.result.result.is_err(),
        "transaction should have failed"
    );
    let escrow_final_balance = env.get_account(escrow).lamports();
    assert_eq!(
        escrow_final_balance,
        escrow_initial_balance - ExecutionTestEnv::BASE_FEE,
        "escrow account should be charged a fee for a failed transaction"
    );
}

/// Verifies that in zero-fee ("gasless") mode, transactions are processed
/// successfully even when the fee payer is a non-delegated account.
#[tokio::test]
async fn test_transaction_gasless_mode() {
    // Initialize the environment with a base fee of 0.
    let env = ExecutionTestEnv::new_with_fee(0);
    let mut payer = env.get_payer();
    payer.set_lamports(1); // Not enough to cover standard fee
    payer.set_delegated(false); // Explicitly set the payer as NON-delegated.
    let initial_balance = payer.lamports();
    payer.commmit();

    let ix = Instruction::new_with_bincode(
        guinea::ID,
        &GuineaInstruction::PrintSizes,
        vec![],
    );
    let txn = env.build_transaction(&[ix]);
    let signature = txn.signatures[0];

    // In a normal fee-paying mode, this execution would fail.
    env.execute_transaction(txn)
        .await
        .expect("transaction should succeed in gasless mode");

    // Verify the transaction was fully processed and broadcast successfully.
    let status = env
        .dispatch
        .transaction_status
        .recv_timeout(Duration::from_millis(100))
        .expect("should receive a transaction status update");

    assert_eq!(status.signature, signature);
    assert!(
        status.result.result.is_ok(),
        "Transaction execution should be successful"
    );

    // Verify that absolutely no fee was charged.
    let final_balance = env.get_payer().lamports();
    assert_eq!(
        initial_balance, final_balance,
        "payer balance should not change in gasless mode"
    );
}

/// Verifies that in zero-fee ("gasless") mode, transactions are processed
/// successfully when using a not existing accounts (not the feepayer).
#[tokio::test]
async fn test_transaction_gasless_mode_with_not_existing_account() {
    // Initialize the environment with a base fee of 0.
    let env = ExecutionTestEnv::new_with_fee(0);
    let mut payer = env.get_payer();
    payer.set_lamports(1); // Not enough to cover standard fee
    payer.set_delegated(false); // Explicitly set the payer as NON-delegated.
    let initial_balance = payer.lamports();
    payer.commmit();

    let ix = Instruction::new_with_bincode(
        guinea::ID,
        &GuineaInstruction::PrintSizes,
        vec![AccountMeta {
            pubkey: Keypair::new().pubkey(),
            is_signer: false,
            is_writable: false,
        }],
    );
    let txn = env.build_transaction(&[ix]);
    let signature = txn.signatures[0];

    // In a normal fee-paying mode, this execution would fail.
    env.execute_transaction(txn)
        .await
        .expect("transaction should succeed in gasless mode");

    // Verify the transaction was fully processed and broadcast successfully.
    let status = env
        .dispatch
        .transaction_status
        .recv_timeout(Duration::from_millis(100))
        .expect("should receive a transaction status update");

    assert_eq!(status.signature, signature);
    assert!(
        status.result.result.is_ok(),
        "Transaction execution should be successful"
    );

    // Verify that absolutely no fee was charged.
    let final_balance = env.get_payer().lamports();
    assert_eq!(
        initial_balance, final_balance,
        "payer balance should not change in gasless mode"
    );
}

/// Verifies that in zero-fee ("gasless") mode, transactions are processed
/// successfully even when the fee payer does not exists.
#[tokio::test]
async fn test_transaction_gasless_mode_not_existing_feepayer() {
    // Initialize the environment with a base fee of 0.
    let payer = Keypair::new();
    let env = ExecutionTestEnv::new_with_payer_and_fees(&payer, 0);

    // Simple noop instruction that does not touch the fee payer account
    let ix = Instruction::new_with_bincode(
        guinea::ID,
        &GuineaInstruction::PrintSizes,
        vec![],
    );
    let txn = env.build_transaction(&[ix]);
    let signature = txn.signatures[0];

    // In a normal fee-paying mode, this execution would fail.
    env.execute_transaction(txn)
        .await
        .expect("transaction should succeed in gasless mode");

    // Verify the transaction was fully processed and broadcast successfully.
    let status = env
        .dispatch
        .transaction_status
        .recv_timeout(Duration::from_millis(100))
        .expect("should receive a transaction status update");

    assert_eq!(status.signature, signature);
    assert!(
        status.result.result.is_ok(),
        "Transaction execution should be successful"
    );

    // Verify that the payer balance is zero (or doesn't exist)
    let final_balance = env
        .accountsdb
        .get_account(&payer.pubkey())
        .unwrap_or_default()
        .lamports();
    assert_eq!(
        final_balance, 0,
        "payer balance of a not existing feepayer should be 0 in gasless mode"
    );
}
