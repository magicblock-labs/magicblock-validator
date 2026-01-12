use std::{collections::HashSet, time::Duration};

use guinea::GuineaInstruction;
use solana_account::{ReadableAccount, WritableAccount};
use solana_keypair::Keypair;
use solana_program::{
    instruction::{AccountMeta, Instruction},
    native_token::LAMPORTS_PER_SOL,
    rent::Rent,
};
use solana_pubkey::Pubkey;
use solana_transaction_error::TransactionError;
use test_kit::{ExecutionTestEnv, Signer};

pub const DELEGATION_PROGRAM_ID: Pubkey =
    Pubkey::from_str_const("DELeGGvXpWV2fqJUhqcF5ZSYMS4JTLjteaAMARRSaeSh");

const BASE_FEE: u64 = ExecutionTestEnv::BASE_FEE;
const TIMEOUT: Duration = Duration::from_millis(100);

/// Derives the ephemeral balance PDA for a given payer.
fn derive_ephemeral_pda(payer: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &[b"balance", payer.as_ref(), &[0]],
        &DELEGATION_PROGRAM_ID,
    )
    .0
}

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
async fn test_escrowed_payer_success() {
    let env = ExecutionTestEnv::new();
    let mut payer = env.get_payer();
    // Ensure primary payer cannot pay standard fee to force escrow usage logic check
    payer.set_lamports(BASE_FEE - 1);
    payer.set_delegated(false);
    let escrow = derive_ephemeral_pda(&payer.pubkey);
    payer.commit();

    env.fund_account(escrow, LAMPORTS_PER_SOL);
    let initial_escrow_bal = env.get_account(escrow).lamports();

    let ix = setup_guinea_ix(&env, GuineaInstruction::PrintSizes);
    let txn = env.build_transaction(&[ix]);

    env.execute_transaction(txn)
        .await
        .expect("Escrow transaction failed");

    assert_eq!(
        env.get_account(escrow).lamports(),
        initial_escrow_bal - BASE_FEE,
        "Escrow should pay fee"
    );

    // Verify updates
    let mut updates = HashSet::new();
    while let Ok(acc) = env.dispatch.account_update.try_recv() {
        updates.insert(acc.account.pubkey);
    }
    assert!(updates.contains(&escrow), "Escrow update missing");
    assert!(
        !updates.contains(&env.get_payer().pubkey),
        "Primary payer update unexpected"
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
    assert!(status.result.result.is_err(), "Transaction should fail");
    assert_eq!(
        env.get_payer().lamports(),
        initial_bal - BASE_FEE,
        "Fee should be charged on failure"
    );
}

#[tokio::test]
async fn test_escrow_charged_for_failed_transaction() {
    let env = ExecutionTestEnv::new();
    let mut payer = env.get_payer();
    payer.set_lamports(0);
    payer.set_delegated(false);
    let escrow = derive_ephemeral_pda(&payer.pubkey);
    payer.commit();

    env.fund_account(escrow, LAMPORTS_PER_SOL);
    let initial_escrow_bal = env.get_account(escrow).lamports();

    // Setup failing instruction (write to empty account)
    let ix = setup_guinea_ix(&env, GuineaInstruction::WriteByteToData(42));
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
    assert!(status.result.result.is_err(), "Transaction should fail");
    assert_eq!(
        env.get_account(escrow).lamports(),
        initial_escrow_bal - BASE_FEE,
        "Escrow should be charged on failure"
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
    assert_eq!(status.signature, sig);
    assert!(status.result.result.is_ok());
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
    assert!(status.result.result.is_ok());
    assert_eq!(
        env.get_payer().lamports(),
        initial_bal,
        "Balance changed in gasless mode"
    );
}
