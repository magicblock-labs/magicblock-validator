use std::time::Duration;

use guinea::GuineaInstruction;
use solana_account::{ReadableAccount, WritableAccount};
use solana_compute_budget_interface::ComputeBudgetInstruction;
use solana_keypair::Keypair;
use solana_program::{
    instruction::{AccountMeta, Instruction},
    native_token::LAMPORTS_PER_SOL,
    rent::Rent,
};
use test_kit::{ExecutionTestEnv, Signer};

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
    assert_eq!(env.get_payer().lamports(), initial_payer_bal);
}

#[tokio::test]
async fn test_compute_unit_price_does_not_change_fee() {
    let env = ExecutionTestEnv::new();
    let initial_payer_bal = env.get_payer().lamports();

    let compute_limit_ix =
        ComputeBudgetInstruction::set_compute_unit_limit(1_400_000);
    let compute_price_ix = ComputeBudgetInstruction::set_compute_unit_price(1);
    let guinea_ix = Instruction::new_with_bincode(
        guinea::ID,
        &GuineaInstruction::PrintSizes,
        vec![],
    );
    let txn =
        env.build_transaction(&[compute_limit_ix, compute_price_ix, guinea_ix]);

    env.execute_transaction(txn)
        .await
        .expect("Transaction with CU price failed");

    let status = env
        .dispatch
        .transaction_status
        .recv_timeout(TIMEOUT)
        .unwrap();
    assert!(status.meta.status.is_ok());
    assert_eq!(status.meta.fee, 0);
    assert_eq!(env.get_payer().lamports(), initial_payer_bal);
}

#[tokio::test]
async fn test_failed_transaction_does_not_commit_accounts() {
    let env = ExecutionTestEnv::new();
    env.wait_for_scheduler_ready().await;
    let initial_bal = env.get_payer().lamports();
    let sender =
        env.create_account_with_config(LAMPORTS_PER_SOL, 0, guinea::ID);
    let recipient = env.create_account(LAMPORTS_PER_SOL);
    const AMOUNT: u64 = 1_000_000;
    let transfer_ix = Instruction::new_with_bincode(
        guinea::ID,
        &GuineaInstruction::Transfer(AMOUNT),
        vec![
            AccountMeta::new(sender.pubkey(), false),
            AccountMeta::new(recipient.pubkey(), false),
        ],
    );

    // Create invalid instruction (writing to empty data)
    let fail_ix = setup_guinea_ix(&env, GuineaInstruction::WriteByteToData(42));
    // Hack: manually set data len to 0 to force failure
    let mut acc = env.get_account(fail_ix.accounts[0].pubkey);
    acc.set_data(vec![]);
    env.accountsdb
        .insert_account(&fail_ix.accounts[0].pubkey, &acc)
        .unwrap();
    let target = fail_ix.accounts[0].pubkey;

    let txn = env.build_transaction(&[transfer_ix, fail_ix]);
    env.transaction_scheduler.schedule(txn).await.unwrap();

    let status = env
        .dispatch
        .transaction_status
        .recv_timeout(TIMEOUT)
        .unwrap();
    assert!(status.meta.status.is_err(), "Transaction should fail");
    assert_eq!(status.meta.fee, 0);
    assert_eq!(
        env.get_payer().lamports(),
        initial_bal,
        "Failed transaction changed the payer"
    );
    assert_eq!(
        env.get_account(sender.pubkey()).lamports(),
        LAMPORTS_PER_SOL
    );
    assert_eq!(
        env.get_account(recipient.pubkey()).lamports(),
        LAMPORTS_PER_SOL
    );
    assert!(env.get_account(target).data().is_empty());
    assert!(
        env.dispatch.account_update.try_recv().is_err(),
        "Failed transaction emitted an account update"
    );
}

#[tokio::test]
async fn test_transaction_gasless_mode() {
    let env = ExecutionTestEnv::new_with_config(1, false);
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
async fn test_transaction_gasless_mode_with_cu_price() {
    let env = ExecutionTestEnv::new_with_config(1, false);
    let mut payer = env.get_payer();
    payer.set_lamports(1);
    payer.set_delegated(false);
    let initial_bal = payer.lamports();
    payer.commit();

    let compute_limit_ix =
        ComputeBudgetInstruction::set_compute_unit_limit(1_400_000);
    let compute_price_ix = ComputeBudgetInstruction::set_compute_unit_price(1);
    let guinea_ix = Instruction::new_with_bincode(
        guinea::ID,
        &GuineaInstruction::PrintSizes,
        vec![],
    );
    let txn =
        env.build_transaction(&[compute_limit_ix, compute_price_ix, guinea_ix]);
    let sig = txn.signatures[0];

    env.execute_transaction(txn)
        .await
        .expect("Gasless tx with CU price failed");

    let status = env
        .dispatch
        .transaction_status
        .recv_timeout(TIMEOUT)
        .unwrap();
    assert_eq!(status.txn.signatures()[0], sig);
    assert!(status.meta.status.is_ok());
    assert_eq!(status.meta.fee, 0);
    assert_eq!(
        env.get_payer().lamports(),
        initial_bal,
        "Balance changed in gasless mode"
    );
}

#[tokio::test]
async fn test_transaction_gasless_mode_with_non_existent_account() {
    let env = ExecutionTestEnv::new_with_config(1, false);
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
