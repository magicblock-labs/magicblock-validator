use std::time::Duration;

use guinea::GuineaInstruction;
use solana_account::ReadableAccount;
use solana_program::{
    instruction::{AccountMeta, Instruction},
    native_token::LAMPORTS_PER_SOL,
};
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use solana_transaction::versioned::VersionedTransaction;
use test_kit::ExecutionTestEnv;

const ACCOUNTS_COUNT: usize = 8;

/// A test helper that creates a specific state for replay testing.
///
/// It achieves a state where a transaction is present in the ledger, but its
/// effects are not yet reflected in the `AccountsDb`. This simulates a scenario
/// like a validator restarting and needing to catch up.
///
/// 1. Executes a transaction, which updates both the ledger and `AccountsDb`.
/// 2. Takes a snapshot of the accounts *before* the transaction.
/// 3. Reverts the accounts in `AccountsDb` to their pre-transaction state.
/// 4. Drains any broadcast channels to ensure a clean test state.
async fn create_transaction_in_ledger(
    env: &ExecutionTestEnv,
    metafn: fn(Pubkey, bool) -> AccountMeta,
    ix: GuineaInstruction,
) -> (VersionedTransaction, Vec<Pubkey>) {
    let accounts: Vec<_> = (0..ACCOUNTS_COUNT)
        .map(|_| {
            env.create_account_with_config(LAMPORTS_PER_SOL, 128, guinea::ID)
        })
        .collect();
    let account_metas: Vec<_> =
        accounts.iter().map(|a| metafn(a.pubkey(), false)).collect();
    let pubkeys: Vec<_> = account_metas.iter().map(|m| m.pubkey).collect();

    // Take a snapshot of accounts before the transaction.
    let pre_account_states: Vec<_> = pubkeys
        .iter()
        .map(|pubkey| {
            let mut acc = env.accountsdb.get_account(pubkey).unwrap();
            acc.ensure_owned();
            (*pubkey, acc)
        })
        .collect();

    // Build and execute the transaction to commit it to the ledger.
    let ix = Instruction::new_with_bincode(guinea::ID, &ix, account_metas);
    let txn = env.build_transaction(&[ix]);
    let sig = txn.signatures[0];
    env.execute_transaction(txn.clone()).await.unwrap();

    // Revert accounts to their previous state to simulate `AccountsDb` being behind the ledger.
    for (pubkey, acc) in &pre_account_states {
        env.accountsdb.insert_account(pubkey, acc);
    }

    // Confirm the transaction is in the ledger and retrieve it.
    let transaction = env
        .ledger
        .get_complete_transaction(sig, u64::MAX)
        .unwrap()
        .unwrap()
        .get_transaction();

    // Drain dispatch channels for a clean test.
    while env.dispatch.transaction_status.try_recv().is_ok() {}
    while env.dispatch.account_update.try_recv().is_ok() {}

    (transaction, pubkeys)
}

/// Verifies that `replay_transaction` correctly applies state changes to the
/// `AccountsDb` without broadcasting any external notifications.
#[tokio::test]
pub async fn test_replay_state_transition() {
    let env = ExecutionTestEnv::new();
    let (transaction, pubkeys) = create_transaction_in_ledger(
        &env,
        AccountMeta::new, // Accounts are writable
        GuineaInstruction::WriteByteToData(42),
    )
    .await;

    // Verify that accounts are in their original state before the replay.
    for pubkey in &pubkeys {
        let account = env.accountsdb.get_account(pubkey).unwrap();
        assert_eq!(account.data()[0], 0);
    }

    // Replay the transaction.
    let result = env.replay_transaction(transaction).await;
    assert!(result.is_ok(), "transaction replay should have succeeded");

    // Verify that replaying does NOT trigger external notifications.
    let status_update = env
        .dispatch
        .transaction_status
        .recv_timeout(Duration::from_millis(100));
    assert!(
        status_update.is_err(),
        "transaction replay should not trigger a signature status update"
    );
    assert!(
        env.dispatch.account_update.try_recv().is_err(),
        "transaction replay should not trigger an account update notification"
    );

    // Verify that the replay resulted in the correct `AccountsDb` state transition.
    for pubkey in &pubkeys {
        let account = env.accountsdb.get_account(pubkey).unwrap();
        assert_eq!(
            account.data()[0],
            42,
            "account data should be modified after replay"
        );
    }
}
