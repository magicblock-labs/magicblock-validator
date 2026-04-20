use std::time::Duration;

use guinea::GuineaInstruction;
use solana_account::{ReadableAccount, WritableAccount};
use solana_keypair::Keypair;
use solana_program::{
    instruction::{AccountMeta, Instruction},
    native_token::LAMPORTS_PER_SOL,
    rent::Rent,
};
use solana_transaction_error::TransactionError;
use test_kit::{ExecutionTestEnv, Signer};

const BASE_FEE: u64 = ExecutionTestEnv::BASE_FEE;
const TIMEOUT: Duration = Duration::from_millis(100);

/// Helper to setup a guinea instruction with a new account.
fn setup_guinea_ix(
    env: &ExecutionTestEnv,
    ix_data: GuineaInstruction,
) -> Instruction {
    let balance = Rent::default().minimum_balance(128);
    let account = env
        .create_account_with_config(balance, 128, guinea::ID)
        .pubkey();

    Instruction::new_with_bincode(
        guinea::ID,
        &ix_data,
        vec![AccountMeta::new(account, false)],
    )
}

#[tokio::test]
async fn test_insufficient_fee() {
    let env = ExecutionTestEnv::new();
    let mut payer = env.get_payer();
    payer.set_lamports(BASE_FEE - 1);
    payer.commit();

    let ix = setup_guinea_ix(&env, GuineaInstruction::PrintSizes);
    let txn = env.build_transaction(&[ix]);

    let result = env.execute_transaction(txn).await;
    assert!(matches!(
        result,
        Err(TransactionError::InsufficientFundsForFee)
    ));
}

#[tokio::test]
async fn test_separate_fee_payer() {
    let env = ExecutionTestEnv::new();
    let sender =
        env.create_account_with_config(LAMPORTS_PER_SOL, 0, guinea::ID);
    let recipient = env.create_account(LAMPORTS_PER_SOL);
    let initial_payer_bal = env.get_payer().lamports();
    const AMOUNT: u64 = 1_000_000;

    let ix = Instruction::new_with_bincode(
        guinea::ID,
        &GuineaInstruction::Transfer(AMOUNT),
        vec![
            AccountMeta::new(sender.pubkey(), false),
            AccountMeta::new(recipient.pubkey(), false),
        ],
    );
    let txn = env.build_transaction(&[ix]);

    env.execute_transaction(txn).await.expect("Transfer failed");

    assert_eq!(
        env.get_account(sender.pubkey()).lamports(),
        LAMPORTS_PER_SOL - AMOUNT
    );
    assert_eq!(
        env.get_account(recipient.pubkey()).lamports(),
        LAMPORTS_PER_SOL + AMOUNT
    );
    assert_eq!(env.get_payer().lamports(), initial_payer_bal - BASE_FEE);
}

#[tokio::test]
async fn test_non_delegated_payer_rejection() {
    let env = ExecutionTestEnv::new();
    let mut payer = env.get_payer();
    payer.set_delegated(false);
    let initial_bal = payer.lamports();
    payer.commit();

    let ix = setup_guinea_ix(&env, GuineaInstruction::PrintSizes);
    let txn = env.build_transaction(&[ix]);

    let result = env.execute_transaction(txn).await;
    assert!(
        matches!(result, Err(TransactionError::InvalidAccountForFee)),
        "Non-delegated payer should be rejected"
    );
    assert_eq!(
        env.get_payer().lamports(),
        initial_bal,
        "Rejected tx should not be charged"
    );
}

#[tokio::test]
async fn test_fee_charged_for_failed_transaction() {
    let env = ExecutionTestEnv::new();
    let initial_bal = env.get_payer().lamports();

    // Create invalid instruction (writing to empty data)
    let ix = setup_guinea_ix(&env, GuineaInstruction::WriteByteToData(42));
    // Hack: manually set data len to 0 to force failure
    let mut acc = env.get_account(ix.accounts[0].pubkey);
    acc.set_data(vec![]);
    env.accountsdb
        .insert_account(&ix.accounts[0].pubkey, &acc)
        .unwrap();

    let txn = env.build_transaction(&[ix]);
    env.transaction_scheduler.schedule(txn).await.unwrap();

    let status = env
        .dispatch
        .transaction_status
        .recv_timeout(TIMEOUT)
        .unwrap();
    assert!(status.meta.status.is_err(), "Transaction should fail");
    assert_eq!(
        env.get_payer().lamports(),
        initial_bal - BASE_FEE,
        "Fee should be charged on failure"
    );
}

#[tokio::test]
async fn test_transaction_gasless_mode() {
    let env = ExecutionTestEnv::new_with_config(0, 1, false);
    let mut payer = env.get_payer();
    payer.set_lamports(1);
    payer.set_delegated(false);
    let initial_bal = payer.lamports();
    payer.commit();

    let ix = Instruction::new_with_bincode(
        guinea::ID,
        &GuineaInstruction::PrintSizes,
        vec![],
    );
    let txn = env.build_transaction(&[ix]);
    let sig = txn.signatures[0];

    env.execute_transaction(txn)
        .await
        .expect("Gasless tx failed");

    let status = env
        .dispatch
        .transaction_status
        .recv_timeout(TIMEOUT)
        .unwrap();
    assert_eq!(status.txn.signatures()[0], sig);
    assert!(status.meta.status.is_ok());
    assert_eq!(
        env.get_payer().lamports(),
        initial_bal,
        "Balance changed in gasless mode"
    );
}

#[tokio::test]
async fn test_transaction_gasless_mode_with_non_existent_account() {
    let env = ExecutionTestEnv::new_with_config(0, 1, false);
    let mut payer = env.get_payer();
    payer.set_lamports(1);
    payer.set_delegated(false);
    let initial_bal = payer.lamports();
    payer.commit();

    // Use a random non-existent account
    let ix = Instruction::new_with_bincode(
        guinea::ID,
        &GuineaInstruction::PrintSizes,
        vec![AccountMeta::new_readonly(Keypair::new().pubkey(), false)],
    );
    let txn = env.build_transaction(&[ix]);

    env.execute_transaction(txn)
        .await
        .expect("Gasless tx with missing acc failed");

    let status = env
        .dispatch
        .transaction_status
        .recv_timeout(TIMEOUT)
        .unwrap();
    assert!(status.meta.status.is_ok());
    assert_eq!(
        env.get_payer().lamports(),
        initial_bal,
        "Balance changed in gasless mode"
    );
}
