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
    let accounts: Vec<_> =
        accounts.iter().map(|a| metafn(a.pubkey(), false)).collect();
    let pubkeys: Vec<_> = accounts.iter().map(|m| m.pubkey).collect();
    let ix = Instruction::new_with_bincode(guinea::ID, &ix, accounts);
    let txn = env.build_transaction(&[ix]);
    let sig = txn.signatures[0];
    // take snapshot of accounts before the transaction
    let pre_account_states: Vec<_> = pubkeys
        .iter()
        .map(|pubkey| {
            let mut acc = env.accountsdb.get_account(pubkey).unwrap();
            acc.ensure_owned();
            (*pubkey, acc)
        })
        .collect();
    // put transaction into ledger
    env.execute_transaction(txn).await.unwrap();
    // revert accounts to previous state, to simulate situation when
    // accountsdb and ledger are out of sync, with accountsdb being behind
    for (pubkey, acc) in &pre_account_states {
        env.accountsdb.insert_account(pubkey, acc);
    }
    // make sure that transaction we just executed is in the ledger
    let transaction = env
        .ledger
        .get_complete_transaction(sig, u64::MAX)
        .unwrap()
        .unwrap();

    // drain dispatch channels for clean experiment
    while env.dispatch.transaction_status.try_recv().is_ok() {}
    while env.dispatch.account_update.try_recv().is_ok() {}

    (transaction.get_transaction(), pubkeys)
}

#[tokio::test]
pub async fn test_replay_state_transition() {
    let env = ExecutionTestEnv::new();
    let (transaction, pubkeys) = create_transaction_in_ledger(
        &env,
        AccountMeta::new,
        GuineaInstruction::WriteByteToData(42),
    )
    .await;

    for pubkey in &pubkeys {
        let account = env.accountsdb.get_account(pubkey).unwrap();
        // accounts are in their original state before replay
        assert!(account.data().first().map(|&b| b == 0).unwrap_or(true));
    }
    let result = env.replay_transaction(transaction).await;
    assert!(result.is_ok(), "transaction replay should have succeeded");

    let status = env
        .dispatch
        .transaction_status
        .recv_timeout(Duration::from_millis(200));
    assert!(
        env.dispatch.account_update.try_recv().is_err(),
        "transaction replay should not have triggered account update notification"
    );
    assert!(
        status.is_err(),
        "transaction replay should not have triggered signature status update"
    );
    for pubkey in &pubkeys {
        let account = env.accountsdb.get_account(pubkey).unwrap();
        assert!(
            account.data().first().map(|&b| b == 42).unwrap_or(true),
            "transaction replay should have resulted in accountsdb state transition"
        );
    }
}
