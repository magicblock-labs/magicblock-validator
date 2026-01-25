use std::{
    collections::{HashMap, HashSet},
    time::{Duration, Instant},
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
use tokio::time::timeout;

const TIMEOUT: Duration = Duration::from_secs(5);
const STRESS_TIMEOUT: Duration = Duration::from_secs(10);
const DEFAULT_BALANCE: u64 = LAMPORTS_PER_SOL * 10;
const TRANSFER_AMOUNT: u64 = 1000;

// --- Helpers ---

fn setup_env(executors: u32) -> ExecutionTestEnv {
    ExecutionTestEnv::new_with_config(
        ExecutionTestEnv::BASE_FEE,
        executors,
        true,
    )
}

fn create_accounts(env: &mut ExecutionTestEnv, count: usize) -> Vec<Pubkey> {
    (0..count)
        .map(|_| {
            env.create_account_with_config(DEFAULT_BALANCE, 128, guinea::ID)
                .pubkey()
        })
        .collect()
}

fn tx_transfer(
    env: &mut ExecutionTestEnv,
    from: Pubkey,
    to: Pubkey,
) -> Transaction {
    let ix = Instruction::new_with_bincode(
        guinea::ID,
        &GuineaInstruction::Transfer(TRANSFER_AMOUNT),
        vec![AccountMeta::new(from, false), AccountMeta::new(to, false)],
    );
    env.build_transaction(&[ix])
}

fn tx_write(
    env: &mut ExecutionTestEnv,
    account: Pubkey,
    val: u8,
) -> Transaction {
    let ix = Instruction::new_with_bincode(
        guinea::ID,
        &GuineaInstruction::WriteByteToData(val),
        vec![AccountMeta::new(account, false)],
    );
    env.build_transaction(&[ix])
}

fn tx_read(env: &mut ExecutionTestEnv, accounts: &[Pubkey]) -> Transaction {
    let metas = accounts
        .iter()
        .map(|k| AccountMeta::new_readonly(*k, false))
        .collect();
    let ix = Instruction::new_with_bincode(
        guinea::ID,
        &GuineaInstruction::PrintSizes,
        metas,
    );
    env.build_transaction(&[ix])
}

/// Schedules transactions, starts the scheduler, and returns signatures in scheduling order.
async fn schedule(
    env: &mut ExecutionTestEnv,
    txs: Vec<Transaction>,
) -> Vec<Signature> {
    let mut sigs = Vec::with_capacity(txs.len());
    for tx in txs {
        sigs.push(tx.signatures[0]);
        env.schedule_transaction(tx).await;
    }
    env.run_scheduler();
    env.advance_slot();
    sigs
}

/// Collects execution statuses until all expected signatures are accounted for or timeout.
async fn collect_statuses(
    env: &mut ExecutionTestEnv,
    count: usize,
    limit: Duration,
    context: &str,
) -> Vec<Signature> {
    let start = Instant::now();
    let mut results = Vec::with_capacity(count);
    let mut status_rx = env.dispatch.transaction_status.resubscribe();

    while results.len() < count {
        if start.elapsed() > limit {
            panic!(
                "[{context}] Timeout waiting for transactions. Got {}/{count}.",
                results.len()
            );
        }
        // Short poll interval
        if let Ok(Ok(status)) =
            timeout(Duration::from_millis(100), status_rx.recv()).await
        {
            assert!(
                status.meta.status.is_ok(),
                "[{context}] Transaction {} failed: {:?}",
                status.txn.signatures()[0],
                status.meta.status
            );
            results.push(status.txn.signatures()[0]);
        }
    }
    results
}

/// Verifies that all signatures were processed successfully (order irrelevant).
async fn verify_unordered(
    env: &mut ExecutionTestEnv,
    sigs: &[Signature],
    limit: Duration,
    context: &str,
) {
    let received = collect_statuses(env, sigs.len(), limit, context).await;
    let expected_set: HashSet<_> = sigs.iter().cloned().collect();
    let received_set: HashSet<_> = received.into_iter().collect();

    if expected_set != received_set {
        let missing: Vec<_> = expected_set.difference(&received_set).collect();
        let extra: Vec<_> = received_set.difference(&expected_set).collect();
        panic!("[{context}] Signature mismatch.\nMissing: {missing:?}\nExtra: {extra:?}");
    }
}

/// Verifies that transactions were processed in the exact order they were scheduled.
async fn verify_ordered(
    env: &mut ExecutionTestEnv,
    sigs: &[Signature],
    limit: Duration,
    context: &str,
) {
    let received = collect_statuses(env, sigs.len(), limit, context).await;
    assert_eq!(
        received,
        sigs.to_vec(),
        "[{context}] Execution order mismatch.\nExpected: {sigs:?}\nGot: {received:?}"
    );
}

// --- Scenarios ---

async fn scenario_parallel_transfers(executors: u32) {
    let ctx = format!("Parallel Transfers ({executors} exec)");
    let mut env = setup_env(executors);
    let pairs = 20;
    let accounts = create_accounts(&mut env, pairs * 2);

    let initial: HashMap<_, _> = accounts
        .iter()
        .map(|k| (*k, env.get_account(*k).lamports()))
        .collect();

    let mut txs = Vec::with_capacity(pairs);
    for i in 0..pairs {
        env.advance_slot();
        txs.push(tx_transfer(&mut env, accounts[i * 2], accounts[i * 2 + 1]));
    }

    let sigs = schedule(&mut env, txs).await;
    verify_unordered(&mut env, &sigs, TIMEOUT, &ctx).await;

    for i in 0..pairs {
        let from = accounts[i * 2];
        let to = accounts[i * 2 + 1];
        assert_eq!(
            env.get_account(from).lamports(),
            initial[&from] - TRANSFER_AMOUNT,
            "[{ctx}] Sender balance incorrect"
        );
        assert_eq!(
            env.get_account(to).lamports(),
            initial[&to] + TRANSFER_AMOUNT,
            "[{ctx}] Recipient balance incorrect"
        );
    }
}

async fn scenario_conflicting_transfers(executors: u32) {
    let ctx = format!("Conflicting Transfers ({executors} exec)");
    let mut env = setup_env(executors);
    let senders_count = 10;
    let mut accounts = create_accounts(&mut env, senders_count + 1);
    let recipient = accounts.pop().unwrap();
    let senders = accounts;

    let initial_recip = env.get_account(recipient).lamports();
    let initial_senders: HashMap<_, _> = senders
        .iter()
        .map(|k| (*k, env.get_account(*k).lamports()))
        .collect();

    let mut txs = Vec::with_capacity(senders.len());
    for sender in &senders {
        env.advance_slot();
        txs.push(tx_transfer(&mut env, *sender, recipient));
    }

    let sigs = schedule(&mut env, txs).await;
    verify_unordered(&mut env, &sigs, TIMEOUT, &ctx).await;

    assert_eq!(
        env.get_account(recipient).lamports(),
        initial_recip + (TRANSFER_AMOUNT * senders_count as u64),
        "[{ctx}] Recipient final balance incorrect"
    );
    for s in senders {
        assert_eq!(
            env.get_account(s).lamports(),
            initial_senders[&s] - TRANSFER_AMOUNT,
            "[{ctx}] Sender {s} final balance incorrect"
        );
    }
}

async fn scenario_readonly_parallel(executors: u32) {
    let ctx = format!("Readonly Parallel ({executors} exec)");
    let mut env = setup_env(executors);
    let count = 50;
    let accounts = create_accounts(&mut env, 10);

    let mut txs = Vec::with_capacity(count);
    for i in 0..count {
        env.advance_slot();
        txs.push(tx_read(
            &mut env,
            &[accounts[i % 10], accounts[(i + 1) % 10]],
        ));
    }

    let sigs = schedule(&mut env, txs).await;
    verify_unordered(&mut env, &sigs, TIMEOUT, &ctx).await;
}

async fn scenario_mixed_workload(executors: u32) {
    let ctx = format!("Mixed Workload ({executors} exec)");
    let mut env = setup_env(executors);
    let sets = 10;
    let accs = create_accounts(&mut env, 6); // A..F
    let initial: HashMap<_, _> = accs
        .iter()
        .map(|k| (*k, env.get_account(*k).lamports()))
        .collect();

    let mut txs = Vec::new();
    for _ in 0..sets {
        txs.push(tx_transfer(&mut env, accs[0], accs[1])); // A->B
        txs.push(tx_transfer(&mut env, accs[2], accs[3])); // C->D
        txs.push(tx_transfer(&mut env, accs[1], accs[0])); // B->A (conflict T1)
        txs.push(tx_transfer(&mut env, accs[4], accs[5])); // E->F
        env.advance_slot();
    }

    let sigs = schedule(&mut env, txs).await;
    verify_unordered(&mut env, &sigs, TIMEOUT, &ctx).await;

    let n = sets as u64;
    // A & B net zero (transfer back and forth)
    assert_eq!(
        env.get_account(accs[0]).lamports(),
        initial[&accs[0]],
        "[{ctx}] Account A balance mismatch"
    );
    assert_eq!(
        env.get_account(accs[1]).lamports(),
        initial[&accs[1]],
        "[{ctx}] Account B balance mismatch"
    );

    // C, E lose; D, F gain
    assert_eq!(
        env.get_account(accs[2]).lamports(),
        initial[&accs[2]] - (TRANSFER_AMOUNT * n),
        "[{ctx}] Account C balance mismatch"
    );
}

async fn scenario_conflicting_writes(executors: u32) {
    let ctx = format!("Conflicting Writes ({executors} exec)");
    let mut env = setup_env(executors);
    let count = 20;
    let acc = create_accounts(&mut env, 1)[0];

    let mut txs = Vec::with_capacity(count);
    for i in 1..=count {
        env.advance_slot();
        txs.push(tx_write(&mut env, acc, i as u8));
    }

    let sigs = schedule(&mut env, txs).await;
    verify_unordered(&mut env, &sigs, TIMEOUT, &ctx).await;

    // We can't guarantee *which* write was last, but it must be one of them.
    let data = env.get_account(acc).data()[0];
    assert!(
        data > 0 && data <= count as u8,
        "[{ctx}] Final data value {data} out of expected range (1..={count})"
    );
}

async fn scenario_serial_conflicting_writes(executors: u32) {
    let ctx = format!("Serial Conflicting Writes ({executors} exec)");
    let mut env = setup_env(executors);
    let count = 20;
    let acc = create_accounts(&mut env, 1)[0];

    let mut txs = Vec::with_capacity(count);
    for i in 0..count {
        env.advance_slot();
        txs.push(tx_write(&mut env, acc, i as u8));
    }

    // Verify transactions executed in exact scheduling order due to write lock
    let sigs = schedule(&mut env, txs).await;
    verify_ordered(&mut env, &sigs, TIMEOUT, &ctx).await;

    // Final state must match last write
    let final_val = env.get_account(acc).data()[0];
    assert_eq!(
        final_val,
        (count - 1) as u8,
        "[{ctx}] Final data value mismatch"
    );
}

async fn scenario_serial_transfer_chain(executors: u32) {
    let ctx = format!("Serial Transfer Chain ({executors} exec)");
    let mut env = setup_env(executors);
    let count = 20;
    let accs = create_accounts(&mut env, count + 1);
    let initial: HashMap<_, _> = accs
        .iter()
        .map(|k| (*k, env.get_account(*k).lamports()))
        .collect();

    let mut txs = Vec::with_capacity(count);
    for i in 0..count {
        env.advance_slot();
        txs.push(tx_transfer(&mut env, accs[i], accs[i + 1]));
    }

    let sigs = schedule(&mut env, txs).await;
    verify_ordered(&mut env, &sigs, TIMEOUT, &ctx).await;

    // A loses, Last gains, Middle net zero
    assert_eq!(
        env.get_account(accs[0]).lamports(),
        initial[&accs[0]] - TRANSFER_AMOUNT,
        "[{ctx}] First account balance mismatch"
    );
    assert_eq!(
        env.get_account(accs[count]).lamports(),
        initial[&accs[count]] + TRANSFER_AMOUNT,
        "[{ctx}] Last account balance mismatch"
    );
    for i in 1..count {
        assert_eq!(
            env.get_account(accs[i]).lamports(),
            initial[&accs[i]],
            "[{ctx}] Intermediate account {i} balance changed"
        );
    }
}

async fn scenario_stress_test(executors: u32) {
    let ctx = "Large Queue Stress Test";
    let mut env = setup_env(executors);
    let count = 1000;
    let num_accs = 100;

    let t_accs = create_accounts(&mut env, num_accs);
    let g_accs = create_accounts(&mut env, num_accs);

    let mut txs = Vec::with_capacity(count);
    for i in 0..count {
        env.advance_slot();
        let idx = i % num_accs;
        let tx = match i % 4 {
            0 => {
                tx_transfer(&mut env, t_accs[idx], t_accs[(idx + 1) % num_accs])
            }
            1 => tx_write(&mut env, g_accs[0], (i % 255) as u8),
            2 => tx_write(&mut env, g_accs[idx], (i % 255) as u8),
            _ => {
                tx_read(&mut env, &[g_accs[idx], g_accs[(idx + 1) % num_accs]])
            }
        };
        txs.push(tx);
    }

    let sigs = schedule(&mut env, txs).await;
    verify_unordered(&mut env, &sigs, STRESS_TIMEOUT, ctx).await;
}

// --- Tests ---

#[tokio::test]
async fn test_parallel_transfers() {
    for executors in [1, 2, 4, 8] {
        scenario_parallel_transfers(executors).await;
    }
}

#[tokio::test]
async fn test_conflicting_transfers() {
    for executors in [1, 2, 4, 8] {
        scenario_conflicting_transfers(executors).await;
    }
}

#[tokio::test]
async fn test_readonly_parallel() {
    for executors in [1, 2, 4, 8] {
        scenario_readonly_parallel(executors).await;
    }
}

#[tokio::test]
async fn test_mixed_workload() {
    for executors in [1, 2, 4, 8] {
        scenario_mixed_workload(executors).await;
    }
}

#[tokio::test]
async fn test_conflicting_writes() {
    for executors in [1, 2, 4, 8] {
        scenario_conflicting_writes(executors).await;
    }
}

#[tokio::test]
async fn test_serial_conflicting_writes() {
    for executors in [1, 2, 4, 8] {
        scenario_serial_conflicting_writes(executors).await;
    }
}

#[tokio::test]
async fn test_serial_transfer_chain() {
    for executors in [1, 2, 4, 8] {
        scenario_serial_transfer_chain(executors).await;
    }
}

#[tokio::test]
async fn test_large_queue_mixed_8_executors() {
    scenario_stress_test(8).await;
}
