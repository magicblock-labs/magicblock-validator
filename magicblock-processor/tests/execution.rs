use std::{collections::HashSet, time::Duration};

use guinea::GuineaInstruction;
use magicblock_core::link::transactions::TransactionResult;
use solana_account::ReadableAccount;
use solana_program::{
    instruction::{AccountMeta, Instruction},
    native_token::LAMPORTS_PER_SOL,
};
use solana_pubkey::Pubkey;
use solana_signature::Signature;
use test_kit::{ExecutionTestEnv, Signer};

const ACCOUNTS_COUNT: usize = 8;

/// A generic helper to execute a transaction with a specific `GuineaInstruction`.
///
/// This function automates the common test pattern of:
/// 1. Creating a set of test accounts.
/// 2. Building an instruction with those accounts.
/// 3. Building and executing the transaction.
/// 4. Advancing the slot to finalize the block.
async fn execute_transaction(
    env: &ExecutionTestEnv,
    metafn: fn(Pubkey, bool) -> AccountMeta,
    ix: GuineaInstruction,
) -> (TransactionResult, Signature) {
    let accounts: Vec<_> = (0..ACCOUNTS_COUNT)
        .map(|_| {
            env.create_account_with_config(LAMPORTS_PER_SOL, 128, guinea::ID)
        })
        .collect();
    let account_metas =
        accounts.iter().map(|a| metafn(a.pubkey(), false)).collect();
    env.advance_slot();

    let ix = Instruction::new_with_bincode(guinea::ID, &ix, account_metas);
    let txn = env.build_transaction(&[ix]);
    let sig = txn.signatures[0];
    let result = env.execute_transaction(txn).await;

    env.advance_slot();
    (result, sig)
}

/// Verifies that transaction return data is correctly captured and persisted in the ledger.
#[tokio::test]
pub async fn test_transaction_with_return_data() {
    let env = ExecutionTestEnv::new();
    let (result, sig) = execute_transaction(
        &env,
        AccountMeta::new_readonly,
        GuineaInstruction::ComputeBalances,
    )
    .await;
    assert!(
        result.is_ok(),
        "failed to execute compute balance transaction"
    );

    let meta = env
        .get_transaction(sig)
        .expect("transaction meta should have been written to the ledger");
    let retdata = meta
        .return_data
        .expect("transaction return data should have been set");
    assert_eq!(
        &retdata.data,
        &(ACCOUNTS_COUNT as u64 * LAMPORTS_PER_SOL).to_le_bytes(),
        "the total balance of accounts should have been placed in return data"
    );
}

/// Verifies that a `TransactionStatus` update, including logs, is broadcast after execution.
#[tokio::test]
pub async fn test_transaction_status_update() {
    let env = ExecutionTestEnv::new();
    let (result, sig) = execute_transaction(
        &env,
        AccountMeta::new_readonly,
        GuineaInstruction::PrintSizes,
    )
    .await;
    assert!(result.is_ok(), "failed to execute print sizes transaction");

    let status = env.dispatch
        .transaction_status
        .recv_timeout(Duration::from_millis(200))
        .expect("transaction status should be delivered immediately after execution");

    assert_eq!(status.signature, sig);
    let logs = status
        .result
        .logs
        .expect("transaction should have produced logs");
    assert!(
        logs.len() > ACCOUNTS_COUNT,
        "should produce more logs than accounts in the transaction"
    );
}

/// Verifies that account modifications are written to the `AccountsDb`
/// and that corresponding `AccountUpdate` notifications are sent.
#[tokio::test]
pub async fn test_transaction_modifies_accounts() {
    let env = ExecutionTestEnv::new();
    let (result, _) = execute_transaction(
        &env,
        AccountMeta::new,
        GuineaInstruction::WriteByteToData(42),
    )
    .await;
    assert!(result.is_ok(), "failed to execute write byte transaction");

    // First, verify the state change directly in the AccountsDb.
    let status = env
        .dispatch
        .transaction_status
        .recv_timeout(Duration::from_millis(200))
        .expect("successful transaction status should be delivered");

    let mut modified_accounts = HashSet::with_capacity(ACCOUNTS_COUNT);
    for acc_pubkey in status.result.accounts.iter().skip(1).take(ACCOUNTS_COUNT)
    {
        let account = env
            .accountsdb
            .get_account(acc_pubkey)
            .expect("transaction account should be in database");
        assert_eq!(
            account.data()[0],
            42,
            "the first byte of the account data should have been modified"
        );
        modified_accounts.insert(*acc_pubkey);
    }

    // Second, verify that account update notifications were broadcast for all modified accounts.
    let mut updated_accounts = HashSet::with_capacity(ACCOUNTS_COUNT);
    // Drain the channel to collect all updates from the single transaction.
    while let Ok(acc) = env.dispatch.account_update.try_recv() {
        updated_accounts.insert(acc.account.pubkey);
    }

    assert!(
        updated_accounts.is_superset(&modified_accounts),
        "account updates should be forwarded for all modified accounts"
    );
}
