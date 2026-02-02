use std::{collections::HashSet, time::Duration};

use guinea::GuineaInstruction;
use solana_account::ReadableAccount;
use solana_program::{
    instruction::{AccountMeta, Instruction},
    native_token::LAMPORTS_PER_SOL,
};
use solana_pubkey::Pubkey;
use solana_signature::Signature;
use test_kit::{ExecutionTestEnv, Signer};

const ACCOUNTS_COUNT: usize = 8;
const TIMEOUT: Duration = Duration::from_millis(200);

/// Helper to execute a standard "Guinea" transaction on `ACCOUNTS_COUNT` new accounts.
async fn execute_guinea(
    env: &ExecutionTestEnv,
    ix: GuineaInstruction,
    is_writable: bool,
) -> (Signature, Vec<Pubkey>) {
    let accounts: Vec<_> = (0..ACCOUNTS_COUNT)
        .map(|_| {
            env.create_account_with_config(LAMPORTS_PER_SOL, 128, guinea::ID)
        })
        .collect();

    let metas = accounts
        .iter()
        .map(|a| {
            if is_writable {
                AccountMeta::new(a.pubkey(), false)
            } else {
                AccountMeta::new_readonly(a.pubkey(), false)
            }
        })
        .collect();

    env.advance_slot();
    let ix = Instruction::new_with_bincode(guinea::ID, &ix, metas);
    let txn = env.build_transaction(&[ix]);
    let sig = txn.signatures[0];

    assert!(
        env.execute_transaction(txn).await.is_ok(),
        "Transaction execution failed"
    );
    env.advance_slot();

    let pubkeys = accounts.iter().map(|a| a.pubkey()).collect();
    (sig, pubkeys)
}

#[tokio::test]
async fn test_transaction_with_return_data() {
    let env = ExecutionTestEnv::new();
    let (sig, _) =
        execute_guinea(&env, GuineaInstruction::ComputeBalances, false).await;

    let meta = env.get_transaction(sig).expect("Transaction not in ledger");
    let ret_data = meta.return_data.expect("Return data missing");

    let expected = (ACCOUNTS_COUNT as u64 * LAMPORTS_PER_SOL).to_le_bytes();
    assert_eq!(ret_data.data, expected, "Incorrect return data balance");
}

#[tokio::test]
async fn test_transaction_status_update() {
    let env = ExecutionTestEnv::new();
    let (sig, _) =
        execute_guinea(&env, GuineaInstruction::PrintSizes, false).await;

    let status = env
        .dispatch
        .transaction_status
        .recv_timeout(TIMEOUT)
        .expect("Status update missing");

    assert_eq!(status.txn.signatures()[0], sig);
    let logs = status.meta.log_messages.as_ref().expect("Logs missing");
    assert!(logs.len() > ACCOUNTS_COUNT, "Insufficient logs produced");
}

#[tokio::test]
async fn test_transaction_modifies_accounts() {
    let env = ExecutionTestEnv::new();
    let (_, accounts) =
        execute_guinea(&env, GuineaInstruction::WriteByteToData(42), true)
            .await;

    // 1. Verify DB state modifications
    let status = env
        .dispatch
        .transaction_status
        .recv_timeout(TIMEOUT)
        .expect("Status update missing");

    // Skip fee payer, check the guinea accounts
    let account_keys: Vec<_> = status
        .txn
        .message()
        .account_keys()
        .iter()
        .copied()
        .collect();
    for pubkey in account_keys.iter().skip(1).take(ACCOUNTS_COUNT) {
        let account = env.get_account(*pubkey);
        assert_eq!(account.data()[0], 42, "Account data mismatch");
    }

    // 2. Verify update notifications
    let mut updated_accounts = HashSet::new();
    while let Ok(update) = env.dispatch.account_update.try_recv() {
        updated_accounts.insert(update.account.pubkey);
    }

    let expected_accounts: HashSet<_> = HashSet::from_iter(accounts);
    assert!(
        updated_accounts.is_superset(&expected_accounts),
        "Missing account update notifications"
    );
}
