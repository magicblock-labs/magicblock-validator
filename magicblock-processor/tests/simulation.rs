use std::time::Duration;

use guinea::GuineaInstruction;
use magicblock_accounts_db::traits::AccountsBank;
use magicblock_core::link::transactions::TransactionSimulationResult;
use solana_account::ReadableAccount;
use solana_compute_budget_interface::ComputeBudgetInstruction;
use solana_instruction::error::InstructionError;
use solana_program::{
    instruction::{AccountMeta, Instruction},
    native_token::LAMPORTS_PER_SOL,
};
use solana_pubkey::Pubkey;
use solana_signature::Signature;
use solana_transaction_error::TransactionError;
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

#[tokio::test]
async fn test_simulation_honors_heap_frame_request() {
    let env = ExecutionTestEnv::new();
    let account = env
        .create_account_with_config(LAMPORTS_PER_SOL, 128, guinea::ID)
        .pubkey();
    let guinea_ix = Instruction::new_with_bincode(
        guinea::ID,
        &GuineaInstruction::PrintSizes,
        vec![AccountMeta::new_readonly(account, false)],
    );

    env.advance_slot();
    let default_heap_txn = env.build_transaction(&[guinea_ix.clone()]);
    let default_heap_result = env.simulate_transaction(default_heap_txn).await;
    assert!(
        default_heap_result.result.is_ok(),
        "default heap simulation failed: {:?}",
        default_heap_result.result
    );

    let heap_frame_ix = ComputeBudgetInstruction::request_heap_frame(64 * 1024);
    env.advance_slot();
    let requested_heap_txn = env.build_transaction(&[heap_frame_ix, guinea_ix]);
    let requested_heap_result =
        env.simulate_transaction(requested_heap_txn).await;
    assert!(
        requested_heap_result.result.is_ok(),
        "requested heap simulation failed: {:?}",
        requested_heap_result.result
    );

    assert!(
        requested_heap_result.units_consumed
            > default_heap_result.units_consumed,
        "requesting a larger heap should consume more units: default={}, requested={}",
        default_heap_result.units_consumed,
        requested_heap_result.units_consumed
    );
}

#[tokio::test]
async fn test_simulation_honors_compute_unit_limit() {
    let env = ExecutionTestEnv::new();
    let account = env
        .create_account_with_config(LAMPORTS_PER_SOL, 128, guinea::ID)
        .pubkey();
    let compute_budget_ix = ComputeBudgetInstruction::set_compute_unit_limit(1);
    let guinea_ix = Instruction::new_with_bincode(
        guinea::ID,
        &GuineaInstruction::PrintSizes,
        vec![AccountMeta::new_readonly(account, false)],
    );

    env.advance_slot();
    let txn = env.build_transaction(&[compute_budget_ix, guinea_ix]);
    let result = env.simulate_transaction(txn).await;

    assert!(
        matches!(
            result.result,
            Err(TransactionError::InstructionError(
                0,
                InstructionError::ComputationalBudgetExceeded
            ))
        ),
        "unexpected result: {:?}",
        result.result
    );
}
