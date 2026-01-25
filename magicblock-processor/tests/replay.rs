use std::time::Duration;

use guinea::GuineaInstruction;
use magicblock_accounts_db::traits::AccountsBank;
use magicblock_core::link::transactions::SanitizeableTransaction;
use solana_account::ReadableAccount;
use solana_program::{
    instruction::{AccountMeta, Instruction},
    native_token::LAMPORTS_PER_SOL,
};
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use solana_transaction::sanitized::SanitizedTransaction;
use test_kit::ExecutionTestEnv;

const ACCOUNTS_COUNT: usize = 8;
const TIMEOUT: Duration = Duration::from_millis(100);

/// Sets up a replay scenario: Transaction is in Ledger, but AccountsDb is reverted to pre-tx state.
async fn setup_replay_scenario(
    env: &ExecutionTestEnv,
    ix: GuineaInstruction,
) -> (SanitizedTransaction, Vec<Pubkey>) {
    // 1. Create Accounts & Build Instruction
    let accounts: Vec<_> = (0..ACCOUNTS_COUNT)
        .map(|_| {
            env.create_account_with_config(LAMPORTS_PER_SOL, 128, guinea::ID)
        })
        .collect();

    let metas = accounts
        .iter()
        .map(|a| AccountMeta::new(a.pubkey(), false))
        .collect();
    let pubkeys: Vec<_> = accounts.iter().map(|a| a.pubkey()).collect();

    // 2. Snapshot Pre-State
    // Critical: ensure_owned() is required to detach the snapshot from the DB's internal ARC,
    // ensuring we hold a distinct copy of the data before modification.
    let pre_state: Vec<_> = pubkeys
        .iter()
        .map(|pk| {
            let mut acc = env.accountsdb.get_account(pk).unwrap();
            acc.ensure_owned();
            (*pk, acc)
        })
        .collect();

    // 3. Execute Transaction (Updates Ledger & DB)
    let ix = Instruction::new_with_bincode(guinea::ID, &ix, metas);
    let txn = env.build_transaction(&[ix]);
    let sig = txn.signatures[0];

    env.execute_transaction(txn).await.unwrap();

    // 4. Revert DB to Pre-State (Simulating "Catch-up" needed)
    for (pk, acc) in pre_state {
        let _ = env.accountsdb.insert_account(&pk, &acc);
    }

    // 5. Fetch Confirmed Transaction from Ledger
    let transaction = env
        .ledger
        .get_complete_transaction(sig, u64::MAX)
        .unwrap()
        .expect("Transaction should be in ledger")
        .get_transaction()
        .sanitize(false)
        .unwrap();

    // 6. Drain channels (Cleanup notifications from the setup execution)
    while env.dispatch.transaction_status.try_recv().is_ok() {}
    while env.dispatch.account_update.try_recv().is_ok() {}

    (transaction, pubkeys)
}

#[tokio::test]
pub async fn test_replay_state_transition() {
    let env = ExecutionTestEnv::new();
    let (txn, pubkeys) =
        setup_replay_scenario(&env, GuineaInstruction::WriteByteToData(42))
            .await;

    // 1. Verify Pre-Replay State (Reverted)
    for pubkey in &pubkeys {
        let account = env.accountsdb.get_account(pubkey).unwrap();
        assert_eq!(account.data()[0], 0, "Account should be in pre-tx state");
    }

    // 2. Perform Replay
    assert!(env.replay_transaction(txn).await.is_ok());

    // 3. Verify No Side Effects (Notifications)
    assert!(
        env.dispatch
            .transaction_status
            .recv_timeout(TIMEOUT)
            .is_err(),
        "Replay should NOT broadcast status updates"
    );
    assert!(
        env.dispatch.account_update.try_recv().is_err(),
        "Replay should NOT broadcast account updates"
    );

    // 4. Verify Post-Replay State (Applied)
    for pubkey in &pubkeys {
        let account = env.accountsdb.get_account(pubkey).unwrap();
        assert_eq!(account.data()[0], 42, "Replay should update account state");
    }
}
