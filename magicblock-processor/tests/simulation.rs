use std::time::Duration;

use guinea::GuineaInstruction;
use magicblock_core::link::transactions::TransactionSimulationResult;
use solana_account::ReadableAccount;
use solana_program::{
    instruction::{AccountMeta, Instruction},
    native_token::LAMPORTS_PER_SOL,
};
use solana_pubkey::Pubkey;
use solana_signature::Signature;
use solana_signer::Signer;
use test_kit::ExecutionTestEnv;

const ACCOUNTS_COUNT: usize = 8;

/// A test helper that builds and simulates a transaction with a specific `GuineaInstruction`.
async fn simulate_transaction(
    env: &ExecutionTestEnv,
    metafn: fn(Pubkey, bool) -> AccountMeta,
    ix: GuineaInstruction,
) -> (TransactionSimulationResult, Signature, Vec<Pubkey>) {
    let accounts: Vec<_> = (0..ACCOUNTS_COUNT)
        .map(|_| {
            env.create_account_with_config(LAMPORTS_PER_SOL, 128, guinea::ID)
        })
        .collect();
    let account_metas: Vec<_> =
        accounts.iter().map(|a| metafn(a.pubkey(), false)).collect();
    let pubkeys = account_metas.iter().map(|m| m.pubkey).collect();
    env.advance_slot();

    let ix = Instruction::new_with_bincode(guinea::ID, &ix, account_metas);
    let txn = env.build_transaction(&[ix]);
    let sig = txn.signatures[0];
    let result = env.simulate_transaction(txn).await;

    env.advance_slot();
    (result, sig, pubkeys)
}

/// Verifies that `simulate_transaction` is a read-only operation with no side effects.
///
/// This test confirms that a simulation does not:
/// 1.  Write the transaction to the ledger.
/// 2.  Modify account state in the `AccountsDb`.
/// 3.  Broadcast any `AccountUpdate` or `TransactionStatus` notifications.
#[tokio::test]
pub async fn test_absent_simulation_side_effects() {
    let env = ExecutionTestEnv::new();
    let (_, sig, pubkeys) = simulate_transaction(
        &env,
        AccountMeta::new, // Accounts are marked as writable for the simulation
        GuineaInstruction::WriteByteToData(42),
    )
    .await;

    // Verify no notifications were sent.
    let status_update = env
        .dispatch
        .transaction_status
        .recv_timeout(Duration::from_millis(100));
    assert!(
        status_update.is_err(),
        "simulation should not trigger a signature status update"
    );
    assert!(
        env.dispatch.account_update.try_recv().is_err(),
        "simulation should not trigger an account update notification"
    );

    // Verify no state was persisted.
    assert!(
        env.get_transaction(sig).is_none(),
        "simulated transaction should not be written to the ledger"
    );
    for pubkey in &pubkeys {
        let account = env.accountsdb.get_account(pubkey).unwrap();
        assert_ne!(
            account.data()[0],
            42,
            "simulation should not modify account state in the database"
        );
    }
}

/// Verifies that a simulation correctly captures execution logs and inner instructions.
#[tokio::test]
pub async fn test_simulation_logs() {
    let env = ExecutionTestEnv::new();
    let (result, _, _) = simulate_transaction(
        &env,
        AccountMeta::new_readonly,
        GuineaInstruction::PrintSizes,
    )
    .await;
    assert!(
        result.result.is_ok(),
        "failed to simulate print sizes transaction"
    );

    let logs = result.logs.expect("simulation should produce logs");
    assert!(
        logs.len() > ACCOUNTS_COUNT,
        "should produce more logs than accounts in the transaction"
    );
    assert!(
        result.inner_instructions.is_some(),
        "simulation should run with CPI recordings enabled"
    );
}

/// Verifies that a simulation correctly captures transaction return data.
#[tokio::test]
pub async fn test_simulation_return_data() {
    let env = ExecutionTestEnv::new();
    let (result, _, _) = simulate_transaction(
        &env,
        AccountMeta::new_readonly,
        GuineaInstruction::ComputeBalances,
    )
    .await;
    assert!(
        result.result.is_ok(),
        "failed to simulate compute balance transaction"
    );

    let retdata = result
        .return_data
        .expect("simulation should run with return data support enabled");
    assert_eq!(
        &retdata.data,
        &(ACCOUNTS_COUNT as u64 * LAMPORTS_PER_SOL).to_le_bytes(),
        "the total balance of accounts should be in the return data"
    );
}
