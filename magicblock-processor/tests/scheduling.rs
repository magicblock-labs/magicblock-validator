use std::{
    collections::{HashMap, HashSet},
    time::Duration,
};

use guinea::GuineaInstruction;
use solana_account::ReadableAccount;
use solana_program::{
    instruction::{AccountMeta, Instruction},
    native_token::LAMPORTS_PER_SOL,
};
use solana_pubkey::Pubkey;
use solana_signature::Signature;
use solana_transaction::Transaction;
use test_kit::{ExecutionTestEnv, Signer};
use tokio::time;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);
const STRESS_TEST_TIMEOUT: Duration = Duration::from_secs(30);
const DEFAULT_LAMPORTS: u64 = LAMPORTS_PER_SOL * 10;
const TRANSFER_AMOUNT: u64 = 1000;
const FEE: u64 = ExecutionTestEnv::BASE_FEE;

// #################################################################
// ## Helpers
// #################################################################

/// Creates an `ExecutionTestEnv` with the specified executor count
/// and `defer_startup` set to `true`.
fn setup_env(executors: u32) -> ExecutionTestEnv {
    ExecutionTestEnv::new_with_config(FEE, executors, true)
}

/// Creates N accounts owned by the `guinea` program.
/// These are used for `WriteByteToData` and `PrintSizes` instructions.
fn create_accounts(env: &ExecutionTestEnv, count: usize) -> Vec<Pubkey> {
    (0..count)
        .map(|_| {
            env.create_account_with_config(DEFAULT_LAMPORTS, 128, guinea::ID)
                .pubkey()
        })
        .collect()
}

/// Builds a `Transfer` transaction from `from` to `to`.
/// The `env.payer` pays the fee.
fn build_transfer_tx(
    env: &ExecutionTestEnv,
    from: &Pubkey,
    to: &Pubkey,
    lamports: u64,
) -> (Transaction, Signature) {
    let ix = Instruction::new_with_bincode(
        guinea::ID,
        &GuineaInstruction::Transfer(lamports),
        vec![AccountMeta::new(*from, false), AccountMeta::new(*to, false)],
    );

    // `env.build_transaction` signs with `env.payer`
    let tx = env.build_transaction(&[ix]);
    let sig = tx.signatures[0];
    (tx, sig)
}

/// Builds a `WriteByteToData` transaction.
/// The `account_pubkey` must be owned by `guinea::ID`.
fn build_write_tx(
    env: &ExecutionTestEnv,
    account_pubkey: &Pubkey,
    value: u8,
) -> (Transaction, Signature) {
    let ix = Instruction::new_with_bincode(
        guinea::ID,
        &GuineaInstruction::WriteByteToData(value),
        vec![AccountMeta::new(*account_pubkey, false)],
    );
    let tx = env.build_transaction(&[ix]);
    let sig = tx.signatures[0];
    (tx, sig)
}

/// Builds a `PrintSizes` (read-only) transaction.
fn build_readonly_tx(
    env: &ExecutionTestEnv,
    accounts: &[Pubkey],
) -> (Transaction, Signature) {
    let metas = accounts
        .iter()
        .map(|pk| AccountMeta::new_readonly(*pk, false))
        .collect();
    let ix = Instruction::new_with_bincode(
        guinea::ID,
        &GuineaInstruction::PrintSizes,
        metas,
    );
    let tx = env.build_transaction(&[ix]);
    let sig = tx.signatures[0];
    (tx, sig)
}

/// Drains the status channel and asserts all expected transactions were successful.
async fn assert_statuses(
    env: &ExecutionTestEnv,
    mut expected_sigs: HashSet<Signature>,
    timeout: Duration,
) {
    let start = std::time::Instant::now();

    while !expected_sigs.is_empty() {
        // Check for overall test timeout
        if start.elapsed() >= timeout {
            panic!(
                "Timeout waiting for transaction statuses. Missing: {:?}",
                expected_sigs
            );
        }

        let recv = env.dispatch.transaction_status.recv_async();
        match time::timeout(Duration::from_millis(100), recv).await {
            Ok(Ok(status)) => {
                // Received a status
                assert!(
                    status.result.result.is_ok(),
                    "Transaction {} failed: {:?}",
                    status.signature,
                    status.result.result
                );
                // Check that we expected this signature
                assert!(
                    expected_sigs.remove(&status.signature),
                    "Received unexpected signature: {}",
                    status.signature
                );
            }
            Ok(Err(e)) => {
                // Channel disconnected
                panic!("Transaction status channel disconnected: {:?}", e);
            }
            Err(_) => {
                // `recv_async` timed out (poll), this is fine, loop again
                continue;
            }
        }
    }
}

/// Drains the status channel and asserts all transactions
/// were successful *and* executed in the exact order specified.
async fn assert_statuses_in_order(
    env: &ExecutionTestEnv,
    expected_sigs_order: Vec<Signature>,
    timeout: Duration,
) {
    let start = std::time::Instant::now();
    let mut received_sigs_order = Vec::with_capacity(expected_sigs_order.len());
    let expected_len = expected_sigs_order.len();

    while received_sigs_order.len() < expected_len {
        // Check for overall test timeout
        if start.elapsed() >= timeout {
            panic!(
                "Timeout waiting for transaction statuses. Expected {} statuses, but only got {}. Missing: {:?}",
                expected_len,
                received_sigs_order.len(),
                &expected_sigs_order[received_sigs_order.len()..]
            );
        }

        let recv = env.dispatch.transaction_status.recv_async();
        match time::timeout(Duration::from_millis(100), recv).await {
            Ok(Ok(status)) => {
                // Received a status
                assert!(
                    status.result.result.is_ok(),
                    "Transaction {} failed: {:?}",
                    status.signature,
                    status.result.result
                );
                received_sigs_order.push(status.signature);
            }
            Ok(Err(e)) => {
                // Channel disconnected
                panic!("Transaction status channel disconnected: {:?}", e);
            }
            Err(_) => {
                // `recv_async` timed out (poll), this is fine, loop again
                continue;
            }
        }
    }

    // Verify the execution order matches the scheduling order.
    assert_eq!(
        received_sigs_order, expected_sigs_order,
        "Transactions were not executed in the expected order."
    );
}

/// Schedules all transactions sequentially and returns their signatures.
async fn schedule_all(
    env: &ExecutionTestEnv,
    txs: Vec<Transaction>,
) -> HashSet<Signature> {
    let sigs = txs.iter().map(|tx| tx.signatures[0]).collect();
    for s in txs.into_iter().map(|tx| env.schedule_transaction(tx)) {
        s.await;
    }
    sigs
}

/// Schedules all transactions sequentially and returns
/// their signatures in an ordered Vec.
async fn schedule_all_and_get_order(
    env: &ExecutionTestEnv,
    txs: Vec<Transaction>,
) -> Vec<Signature> {
    let mut sigs = Vec::with_capacity(txs.len());
    for tx in txs {
        sigs.push(tx.signatures[0]);
        env.schedule_transaction(tx).await;
    }
    sigs
}

// #################################################################
// ## Test Scenarios
// #################################################################

/// **Scenario 1: Parallel Transfers (No Conflicts)**
///
/// Schedules N transactions that do not conflict (e.g., A->B, C->D, E->F).
/// With multiple executors, these should all be processed in parallel.
async fn scenario_parallel_transfers(executors: u32) {
    let mut env = setup_env(executors);
    let num_pairs = 20;
    let accounts = create_accounts(&env, num_pairs * 2);
    let mut initial_balances = HashMap::new();
    let mut txs = vec![];

    for i in 0..num_pairs {
        let from = &accounts[i * 2];
        let to = &accounts[i * 2 + 1];
        initial_balances.insert(from, env.get_account(*from).lamports());
        initial_balances.insert(to, env.get_account(*to).lamports());

        let (tx, _) = build_transfer_tx(&env, from, to, TRANSFER_AMOUNT);
        txs.push(tx);
        // we force a different slot to generate a new signature
        env.advance_slot();
    }

    let sigs = schedule_all(&env, txs).await;
    env.run_scheduler();
    env.advance_slot();

    assert_statuses(&env, sigs.clone(), DEFAULT_TIMEOUT).await;

    // Verify final state
    // Ensure all writes are committed
    for i in 0..num_pairs {
        let from_pk = accounts[i * 2];
        let to_pk = accounts[i * 2 + 1];
        let final_from = env.get_account(from_pk).lamports();
        let final_to = env.get_account(to_pk).lamports();
        let initial_from = initial_balances.get(&from_pk).unwrap();
        let initial_to = initial_balances.get(&to_pk).unwrap();

        // `from` account doesn't pay the fee, `env.payer` does.
        assert_eq!(final_from, initial_from - TRANSFER_AMOUNT);
        assert_eq!(final_to, initial_to + TRANSFER_AMOUNT);
    }

    // Verify ledger
    for sig in sigs {
        assert!(
            env.get_transaction(sig).is_some(),
            "Transaction {sig} not in ledger",
        );
    }
}

/// **Scenario 2: Conflicting Transfers (Write Lock on 1 Account)**
///
/// Schedules N transactions that all write to the *same account* (e.g., B->A, C->A, D->A).
/// The scheduler must serialize these, regardless of executor count.
async fn scenario_conflicting_transfers(executors: u32) {
    let mut env = setup_env(executors);
    let num_senders = 10;
    let mut accounts = create_accounts(&env, num_senders + 1);
    let recipient_pk = accounts.pop().unwrap(); // Account A
    let senders = accounts; // B, C, D...

    let initial_recipient_balance = env.get_account(recipient_pk).lamports();
    let mut initial_sender_balances = HashMap::new();
    let mut txs = vec![];

    for sender in &senders {
        initial_sender_balances
            .insert(sender, env.get_account(*sender).lamports());
        let (tx, _) =
            build_transfer_tx(&env, sender, &recipient_pk, TRANSFER_AMOUNT);
        txs.push(tx);
        // we force a different slot to generate a new signature
        env.advance_slot();
    }

    let sigs = schedule_all(&env, txs).await;
    env.run_scheduler();
    env.advance_slot();

    assert_statuses(&env, sigs, DEFAULT_TIMEOUT).await;

    // Verify final state
    let final_recipient_balance = env.get_account(recipient_pk).lamports();
    let total_received = TRANSFER_AMOUNT * num_senders as u64;
    assert_eq!(
        final_recipient_balance,
        initial_recipient_balance + total_received
    );

    for sender in &senders {
        let final_sender = env.get_account(*sender).lamports();
        let initial_sender = initial_sender_balances.get(sender).unwrap();
        // `sender` account doesn't pay the fee, `env.payer` does.
        assert_eq!(final_sender, initial_sender - TRANSFER_AMOUNT);
    }
}

/// **Scenario 3: Parallel ReadOnly Transactions**
///
/// Schedules N transactions that are all read-only.
/// These should all execute in parallel.
async fn scenario_readonly_parallel(executors: u32) {
    let mut env = setup_env(executors);
    let num_txs = 50;
    let accounts = create_accounts(&env, 10);
    let mut txs = vec![];

    for i in 0..num_txs {
        let acc_idx1 = i % 10;
        let acc_idx2 = (i + 1) % 10;
        let (tx, _) =
            build_readonly_tx(&env, &[accounts[acc_idx1], accounts[acc_idx2]]);
        txs.push(tx);
        // we force a different slot to generate a new signature
        env.advance_slot();
    }

    let sigs = schedule_all(&env, txs).await;
    env.run_scheduler();
    env.advance_slot();

    assert_statuses(&env, sigs, DEFAULT_TIMEOUT).await
}

/// **Scenario 4: Mixed Workload (Conflicts + Parallel)**
///
/// Schedules a mix of conflicting and non-conflicting transactions.
/// T1: A -> B (Conflicts with T3)
/// T2: C -> D (Parallel)
/// T3: B -> A (Conflicts with T1)
/// T4: E -> F (Parallel)
async fn scenario_mixed_workload(executors: u32) {
    let mut env = setup_env(executors);
    let num_sets = 10;
    let accounts = create_accounts(&env, 6);
    let (acc_a, acc_b, acc_c, acc_d, acc_e, acc_f) = (
        &accounts[0],
        &accounts[1],
        &accounts[2],
        &accounts[3],
        &accounts[4],
        &accounts[5],
    );

    let mut initial_balances = HashMap::new();
    for acc in &accounts {
        initial_balances.insert(acc, env.get_account(*acc).lamports());
    }

    let mut txs = vec![];
    for _ in 0..num_sets {
        // T1: A -> B
        let (tx1, _) = build_transfer_tx(&env, acc_a, acc_b, TRANSFER_AMOUNT);
        txs.push(tx1);
        // T2: C -> D
        let (tx2, _) = build_transfer_tx(&env, acc_c, acc_d, TRANSFER_AMOUNT);
        txs.push(tx2);
        // T3: B -> A
        let (tx3, _) = build_transfer_tx(&env, acc_b, acc_a, TRANSFER_AMOUNT);
        txs.push(tx3);
        // T4: E -> F
        let (tx4, _) = build_transfer_tx(&env, acc_e, acc_f, TRANSFER_AMOUNT);
        txs.push(tx4);
        // we force a different slot to generate a new signature
        env.advance_slot();
    }

    let sigs = schedule_all(&env, txs).await;
    env.run_scheduler();
    env.advance_slot();

    assert_statuses(&env, sigs, DEFAULT_TIMEOUT).await;

    // Verify final state
    env.advance_slot();
    let n = num_sets as u64;

    // A and B transferred back and forth `n` times.
    // Their net change is 0, as they are not paying fees.
    assert_eq!(
        env.get_account(*acc_a).lamports(),
        *initial_balances.get(acc_a).unwrap()
    );
    assert_eq!(
        env.get_account(*acc_b).lamports(),
        *initial_balances.get(acc_b).unwrap()
    );
    // C, D, E, F just did one-way transfers. No fees paid by them.
    assert_eq!(
        env.get_account(*acc_c).lamports(),
        initial_balances.get(acc_c).unwrap() - (TRANSFER_AMOUNT * n)
    );
    assert_eq!(
        env.get_account(*acc_d).lamports(),
        initial_balances.get(acc_d).unwrap() + (TRANSFER_AMOUNT * n)
    );
    assert_eq!(
        env.get_account(*acc_e).lamports(),
        initial_balances.get(acc_e).unwrap() - (TRANSFER_AMOUNT * n)
    );
    assert_eq!(
        env.get_account(*acc_f).lamports(),
        initial_balances.get(acc_f).unwrap() + (TRANSFER_AMOUNT * n)
    );
}

/// **Scenario 5: Conflicting Data Writes**
///
/// Schedules N transactions that all write to the *same account's data*
/// using `WriteByteToData`. This creates a write-lock conflict.
async fn scenario_conflicting_writes(executors: u32) {
    let mut env = setup_env(executors);
    let num_txs = 20;
    let guinea_accounts = create_accounts(&env, 1);
    let acc_a = &guinea_accounts[0];
    let mut txs = vec![];
    let mut written_values = HashSet::new();

    for i in 0..num_txs {
        let value = (i + 1) as u8; // Write 1, 2, 3...
        let (tx, _) = build_write_tx(&env, acc_a, value);
        txs.push(tx);
        written_values.insert(value);
        // we force a different slot to generate a new signature
        env.advance_slot();
    }

    let sigs = schedule_all(&env, txs).await;
    env.run_scheduler();
    env.advance_slot();

    assert_statuses(&env, sigs, DEFAULT_TIMEOUT).await;

    // Verify final state
    env.advance_slot();
    let account_data = env.get_account(*acc_a).data().to_vec();
    let final_value = account_data[0];

    // We can't guarantee *which* write was last, but it must be one of them.
    assert!(
        written_values.contains(&final_value),
        "Final value {} was not in the set of written values",
        final_value
    );
}

/// **Scenario 6: Serial Conflicting Writes (Asserts Order)**
///
/// Schedules N transactions that all write to the *same account*.
/// Asserts that they are executed sequentially in the *exact order*
/// they were scheduled, regardless of executor count, due to the write lock.
async fn scenario_serial_conflicting_writes(executors: u32) {
    let mut env = setup_env(executors);
    let num_txs = 20;
    let guinea_accounts = create_accounts(&env, 1);
    let acc_a = &guinea_accounts[0];
    let mut txs = vec![];

    for i in 0..num_txs {
        let value = i as u8; // Write 0, 1, 2... 19
        let (tx, _) = build_write_tx(&env, acc_a, value);
        txs.push(tx);
        // we force a different slot to generate a new signature
        env.advance_slot();
    }

    // Schedule sequentially and get the ordered signatures
    let sigs_order = schedule_all_and_get_order(&env, txs).await;
    env.run_scheduler();
    env.advance_slot();

    // Assert that the statuses were received in the *exact* order of scheduling
    assert_statuses_in_order(&env, sigs_order, DEFAULT_TIMEOUT).await;

    // Verify final state
    env.advance_slot();
    let account_data = env.get_account(*acc_a).data().to_vec();
    let final_value = account_data[0];

    // The final value *must* be the last value we wrote (19)
    assert_eq!(
        final_value,
        (num_txs - 1) as u8,
        "Final account data should be the last value written"
    );
}

/// **Scenario 7: Serial Transfer Chain (Asserts Order)**
///
/// Schedules N transactions in a dependency chain (A->B, B->C, C->D).
/// Asserts that they are executed sequentially in the *exact order*
/// they were scheduled, as each tx depends on the lock from the previous.
async fn scenario_serial_transfer_chain(executors: u32) {
    let mut env = setup_env(executors);
    let num_txs = 20;
    let num_accounts = num_txs + 1;
    let accounts = create_accounts(&env, num_accounts);
    let mut initial_balances = HashMap::new();
    let mut txs = vec![];

    for acc in &accounts {
        initial_balances.insert(acc, env.get_account(*acc).lamports());
    }

    for i in 0..num_txs {
        let from = &accounts[i];
        let to = &accounts[i + 1];
        let (tx, _) = build_transfer_tx(&env, from, to, TRANSFER_AMOUNT);
        txs.push(tx);
        // we force a different slot to generate a new signature
        env.advance_slot();
    }

    // Schedule sequentially and get the ordered signatures
    let sigs_order = schedule_all_and_get_order(&env, txs).await;
    env.run_scheduler();
    env.advance_slot();

    // Assert that the statuses were received in the *exact* order of scheduling
    assert_statuses_in_order(&env, sigs_order, DEFAULT_TIMEOUT).await;

    // Verify final state
    env.advance_slot();
    // First account (A) should have lost lamports
    let first_acc = &accounts[0];
    assert_eq!(
        env.get_account(*first_acc).lamports(),
        initial_balances.get(first_acc).unwrap() - TRANSFER_AMOUNT
    );

    // Intermediary accounts (B, C, ...) should have net-zero change
    for i in 1..num_txs {
        let int_acc = &accounts[i];
        assert_eq!(
            env.get_account(*int_acc).lamports(),
            *initial_balances.get(int_acc).unwrap(),
            "Intermediary account {} lamports changed",
            int_acc
        );
    }

    // Last account should have gained lamports
    let last_acc = &accounts[num_accounts - 1];
    assert_eq!(
        env.get_account(*last_acc).lamports(),
        initial_balances.get(last_acc).unwrap() + TRANSFER_AMOUNT
    );
}

// #################################################################
// ## Test Matrix
// #################################################################

/// Macro to generate tests for different executor counts
macro_rules! test_scenario {
    ($scenario_fn:ident, $test_name:ident, $executors:expr) => {
        #[tokio::test]
        #[allow(non_snake_case)]
        async fn $test_name() {
            $scenario_fn($executors).await;
        }
    };
}

// --- Test Scenario 1: Parallel Transfers (No Conflicts) ---
test_scenario!(
    scenario_parallel_transfers,
    test_parallel_transfers_1_executor,
    1
);
test_scenario!(
    scenario_parallel_transfers,
    test_parallel_transfers_2_executors,
    2
);
test_scenario!(
    scenario_parallel_transfers,
    test_parallel_transfers_4_executors,
    4
);
test_scenario!(
    scenario_parallel_transfers,
    test_parallel_transfers_8_executors,
    8
);

// --- Test Scenario 2: Conflicting Transfers (Write Lock on 1 Account) ---
test_scenario!(
    scenario_conflicting_transfers,
    test_conflicting_transfers_1_executor,
    1
);
test_scenario!(
    scenario_conflicting_transfers,
    test_conflicting_transfers_2_executors,
    2
);
test_scenario!(
    scenario_conflicting_transfers,
    test_conflicting_transfers_4_executors,
    4
);
test_scenario!(
    scenario_conflicting_transfers,
    test_conflicting_transfers_8_executors,
    8
);

// --- Test Scenario 3: Parallel ReadOnly Txs ---
test_scenario!(
    scenario_readonly_parallel,
    test_readonly_parallel_1_executor,
    1
);
test_scenario!(
    scenario_readonly_parallel,
    test_readonly_parallel_2_executors,
    2
);
test_scenario!(
    scenario_readonly_parallel,
    test_readonly_parallel_4_executors,
    4
);
test_scenario!(
    scenario_readonly_parallel,
    test_readonly_parallel_8_executors,
    8
);

// --- Test Scenario 4: Mixed Workload (Conflicts + Parallel) ---
test_scenario!(scenario_mixed_workload, test_mixed_workload_1_executor, 1);
test_scenario!(scenario_mixed_workload, test_mixed_workload_2_executors, 2);
test_scenario!(scenario_mixed_workload, test_mixed_workload_4_executors, 4);
test_scenario!(scenario_mixed_workload, test_mixed_workload_8_executors, 8);

// --- Test Scenario 5: Conflicting Data Writes ---
test_scenario!(
    scenario_conflicting_writes,
    test_conflicting_writes_1_executor,
    1
);
test_scenario!(
    scenario_conflicting_writes,
    test_conflicting_writes_2_executors,
    2
);
test_scenario!(
    scenario_conflicting_writes,
    test_conflicting_writes_4_executors,
    4
);
test_scenario!(
    scenario_conflicting_writes,
    test_conflicting_writes_8_executors,
    8
);

// --- Test Scenario 6: Serial Conflicting Writes (Asserts Order) ---
test_scenario!(
    scenario_serial_conflicting_writes,
    test_serial_conflicting_writes_1_executor,
    1
);
test_scenario!(
    scenario_serial_conflicting_writes,
    test_serial_conflicting_writes_2_executors,
    2
);
test_scenario!(
    scenario_serial_conflicting_writes,
    test_serial_conflicting_writes_4_executors,
    4
);
test_scenario!(
    scenario_serial_conflicting_writes,
    test_serial_conflicting_writes_8_executors,
    8
);

// --- Test Scenario 7: Serial Transfer Chain (Asserts Order) ---
test_scenario!(
    scenario_serial_transfer_chain,
    test_serial_transfer_chain_1_executor,
    1
);
test_scenario!(
    scenario_serial_transfer_chain,
    test_serial_transfer_chain_2_executors,
    2
);
test_scenario!(
    scenario_serial_transfer_chain,
    test_serial_transfer_chain_4_executors,
    4
);
test_scenario!(
    scenario_serial_transfer_chain,
    test_serial_transfer_chain_8_executors,
    8
);

// --- Test Scenario 8: Large Queue Stress Test ---
/// **Scenario 8: Large Queue Stress Test**
///
/// Schedules 1000 transactions of mixed types to check for deadlocks or
/// race conditions under heavy load.
#[tokio::test]
async fn test_large_queue_mixed_8_executors() {
    let mut env = setup_env(8);
    let num_txs = 1000;
    let num_accounts = 100;

    let transfer_accounts = create_accounts(&env, num_accounts);
    let guinea_accounts = create_accounts(&env, num_accounts);
    let mut txs = vec![];

    for i in 0..num_txs {
        let r = i % 4;
        let idx = i % num_accounts;

        let tx = match r {
            // 0: Non-conflicting transfer (A->B, B->C, ...)
            0 => {
                let from = &transfer_accounts[idx];
                let to_idx = (idx + 1) % num_accounts;
                let to = &transfer_accounts[to_idx];
                build_transfer_tx(&env, from, to, 10).0
            }
            // 1: Conflicting write (all write to guinea_accounts[0])
            1 => build_write_tx(&env, &guinea_accounts[0], i as u8).0,
            // 2: Non-conflicting write (A writes A, B writes B, ...)
            2 => build_write_tx(&env, &guinea_accounts[idx], i as u8).0,
            // 3: Readonly
            _ => {
                build_readonly_tx(
                    &env,
                    &[
                        guinea_accounts[idx],
                        guinea_accounts[(idx + 1) % num_accounts],
                    ],
                )
                .0
            }
        };
        txs.push(tx);
        // we force a different slot to generate a new signature
        env.advance_slot();
    }

    let sigs = schedule_all(&env, txs).await;
    env.run_scheduler();
    env.advance_slot();

    // Use a longer timeout for the large queue
    assert_statuses(&env, sigs, STRESS_TEST_TIMEOUT).await;
}
