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
use solana_transaction_status::TransactionStatusMeta;
use test_kit::ExecutionTestEnv;

const ACCOUNTS_COUNT: usize = 8;
const TIMEOUT: Duration = Duration::from_millis(100);

/// Sets up a replay scenario in Replica mode:
/// - Transaction is written directly to Ledger (no execution)
/// - AccountsDb remains in pre-transaction state
fn setup_replay_scenario_replica(
    env: &ExecutionTestEnv,
    ix: GuineaInstruction,
) -> (SanitizedTransaction, Vec<Pubkey>) {
    // 1. Create Accounts
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

    // 2. Build Transaction
    let ix = Instruction::new_with_bincode(guinea::ID, &ix, metas);
    let txn = env.build_transaction(&[ix]);
    let sanitized = txn.sanitize(false).unwrap();
    let sig = *sanitized.signature();

    // 3. Write transaction to ledger directly (without executing)
    // This simulates a transaction that was recorded by the primary
    let meta = TransactionStatusMeta {
        fee: 5000,
        pre_balances: pubkeys.iter().map(|_| LAMPORTS_PER_SOL).collect(),
        post_balances: pubkeys.iter().map(|_| LAMPORTS_PER_SOL).collect(),
        status: Ok(()),
        ..Default::default()
    };
    env.ledger
        .write_transaction(
            sig,
            env.ledger.latest_block().load().slot,
            &sanitized,
            meta,
        )
        .expect("Failed to write transaction to ledger");

    // 4. Verify accounts are still in pre-transaction state
    for pubkey in &pubkeys {
        let account = env.accountsdb.get_account(pubkey).unwrap();
        assert_eq!(
            account.data()[0],
            0,
            "Account should be in pre-tx state before replay"
        );
    }

    (sanitized, pubkeys)
}

#[tokio::test]
pub async fn test_replay_state_transition() {
    // Run in Replica mode (scheduler starts in Replica, no mode switch)
    let env = ExecutionTestEnv::new_replica_mode(1, false);

    let (txn, pubkeys) = setup_replay_scenario_replica(
        &env,
        GuineaInstruction::WriteByteToData(42),
    );

    // 1. Verify Pre-Replay State
    for pubkey in &pubkeys {
        let account = env.accountsdb.get_account(pubkey).unwrap();
        assert_eq!(account.data()[0], 0, "Account should be in pre-tx state");
    }

    // 2. Perform Replay (persist=false: no status notifications)
    assert!(env.replay_transaction(false, txn).await.is_ok());

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
