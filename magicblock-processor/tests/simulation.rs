use std::time::Duration;

use guinea::GuineaInstruction;
use magicblock_accounts_db::traits::AccountsBank;
use magicblock_core::link::transactions::TransactionSimulationResult;
use solana_account::ReadableAccount;
use solana_program::{
    instruction::{AccountMeta, Instruction},
    native_token::LAMPORTS_PER_SOL,
};
use solana_pubkey::Pubkey;
use solana_signature::Signature;
use test_kit::{ExecutionTestEnv, Signer};

const ACCOUNTS_COUNT: usize = 8;
const TIMEOUT: Duration = Duration::from_millis(100);

/// Helper to simulate a standard "Guinea" transaction.
async fn simulate_guinea(
    env: &ExecutionTestEnv,
    ix: GuineaInstruction,
    is_writable: bool,
) -> (TransactionSimulationResult, Signature, Vec<Pubkey>) {
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

    let result = env.simulate_transaction(txn).await;
    let pubkeys = accounts.iter().map(|a| a.pubkey()).collect();

    (result, sig, pubkeys)
}

#[tokio::test]
async fn test_absent_simulation_side_effects() {
    let env = ExecutionTestEnv::new();
    let (_, sig, pubkeys) = simulate_guinea(
        &env,
        GuineaInstruction::WriteByteToData(42),
        true, // writable
    )
    .await;

    // 1. Verify No Notifications
    assert!(
        env.dispatch
            .transaction_status
            .recv_timeout(TIMEOUT)
            .is_err(),
        "Simulation triggered status update"
    );
    assert!(
        env.dispatch.account_update.try_recv().is_err(),
        "Simulation triggered account update"
    );

    // 2. Verify No Persistence
    assert!(
        env.get_transaction(sig).is_none(),
        "Simulated transaction written to ledger"
    );

    // 3. Verify No State Change
    for pubkey in &pubkeys {
        let account = env.accountsdb.get_account(pubkey).unwrap();
        assert_ne!(account.data()[0], 42, "Simulation modified DB state");
    }
}

#[tokio::test]
async fn test_simulation_logs() {
    let env = ExecutionTestEnv::new();
    let (result, _, _) = simulate_guinea(
        &env,
        GuineaInstruction::PrintSizes,
        false, // readonly
    )
    .await;

    assert!(result.result.is_ok(), "Simulation failed");

    let logs = result.logs.expect("Logs missing");
    assert!(logs.len() > ACCOUNTS_COUNT, "Insufficient logs");
    assert!(
        result.inner_instructions.is_some(),
        "Inner instructions missing"
    );
}

#[tokio::test]
async fn test_simulation_return_data() {
    let env = ExecutionTestEnv::new();
    let (result, _, _) = simulate_guinea(
        &env,
        GuineaInstruction::ComputeBalances,
        false, // readonly
    )
    .await;

    assert!(result.result.is_ok(), "Simulation failed");

    let ret_data = result.return_data.expect("Return data missing");
    let expected = (ACCOUNTS_COUNT as u64 * LAMPORTS_PER_SOL).to_le_bytes();
    assert_eq!(ret_data.data, expected, "Incorrect return data");
}
