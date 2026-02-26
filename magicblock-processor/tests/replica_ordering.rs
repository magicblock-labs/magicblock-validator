//! Integration tests for Replica mode transaction ordering enforcement.
//!
//! These tests verify that in Replica mode, when transactions conflict on accounts,
//! they are executed in strict submission order (FIFO), even under concurrent pressure.

use std::{
    collections::HashMap,
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

const STRESS_TIMEOUT: Duration = Duration::from_secs(30);
const DEFAULT_BALANCE: u64 = LAMPORTS_PER_SOL * 10;

// --- Helpers ---

fn setup_replica_env(executors: u32) -> ExecutionTestEnv {
    ExecutionTestEnv::new_replica_mode(executors, true)
}

fn create_accounts(env: &mut ExecutionTestEnv, count: usize) -> Vec<Pubkey> {
    (0..count)
        .map(|_| {
            env.create_account_with_config(DEFAULT_BALANCE, 128, guinea::ID)
                .pubkey()
        })
        .collect()
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

fn tx_transfer(
    env: &mut ExecutionTestEnv,
    from: Pubkey,
    to: Pubkey,
) -> Transaction {
    let ix = Instruction::new_with_bincode(
        guinea::ID,
        &GuineaInstruction::Transfer(1000),
        vec![AccountMeta::new(from, false), AccountMeta::new(to, false)],
    );
    env.build_transaction(&[ix])
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

    while results.len() < count {
        if start.elapsed() > limit {
            panic!(
                "[{context}] Timeout waiting for transactions. Got {}/{}.",
                results.len(),
                count
            );
        }
        if let Ok(Ok(status)) = timeout(
            Duration::from_millis(100),
            env.dispatch.transaction_status.recv_async(),
        )
        .await
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

/// Verifies that transactions were processed in the exact order they were submitted.
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

/// Submits all replay transactions sequentially, then starts the scheduler.
/// Returns signatures in submission order.
async fn submit_all_and_start(
    env: &mut ExecutionTestEnv,
    txs: Vec<Transaction>,
) -> Vec<Signature> {
    let sigs: Vec<Signature> = txs.iter().map(|tx| tx.signatures[0]).collect();

    // Submit all transactions sequentially to preserve order
    for tx in txs {
        env.transaction_scheduler
            .replay(true, tx)
            .await
            .expect("Failed to submit transaction");
    }

    // Now start the scheduler
    env.run_scheduler();
    env.advance_slot();

    sigs
}

// --- Tests ---

/// Stress test: 100 conflicting writes to a single account.
/// In Replica mode, these MUST execute in strict FIFO order.
#[tokio::test]
async fn test_replica_stress_single_account_writes() {
    let ctx = "Replica Stress Single Account";
    let mut env = setup_replica_env(8);
    let count = 100;
    let acc = create_accounts(&mut env, 1)[0];

    let mut txs = Vec::with_capacity(count);
    for i in 0..count {
        env.advance_slot();
        txs.push(tx_write(&mut env, acc, (i % 256) as u8));
    }

    let sigs = submit_all_and_start(&mut env, txs).await;
    verify_ordered(&mut env, &sigs, STRESS_TIMEOUT, ctx).await;

    // Final state must match last write (count - 1)
    let final_val = env.get_account(acc).data()[0];
    assert_eq!(
        final_val,
        ((count - 1) % 256) as u8,
        "[{ctx}] Final data value mismatch"
    );
}

/// Stress test: Multiple accounts with cross-conflicts.
/// Transactions form a chain where each conflicts with the next.
#[tokio::test]
async fn test_replica_stress_conflict_chain() {
    let ctx = "Replica Stress Conflict Chain";
    let mut env = setup_replica_env(8);
    let count = 100;
    let accs = create_accounts(&mut env, count + 1);

    // Build a chain: T0 writes A0+A1, T1 writes A1+A2, etc.
    // Each transaction conflicts with its neighbors
    let mut txs = Vec::with_capacity(count);
    for i in 0..count {
        env.advance_slot();
        txs.push(tx_transfer(&mut env, accs[i], accs[i + 1]));
    }

    let sigs = submit_all_and_start(&mut env, txs).await;
    verify_ordered(&mut env, &sigs, STRESS_TIMEOUT, ctx).await;
}

/// Stress test: Hotspot account with many writers.
/// 50 transactions all write to the same "hot" account plus a unique account each.
#[tokio::test]
async fn test_replica_stress_hotspot() {
    let ctx = "Replica Stress Hotspot";
    let mut env = setup_replica_env(8);
    let count = 50;
    let accs = create_accounts(&mut env, count + 1);
    let hotspot = accs[count]; // The last account is the hotspot

    // Each transaction writes to hotspot + its own account
    let mut txs = Vec::with_capacity(count);
    for (i, acc) in accs.iter().take(count).enumerate() {
        env.advance_slot();
        let ix = Instruction::new_with_bincode(
            guinea::ID,
            &GuineaInstruction::WriteByteToData(i as u8),
            vec![
                AccountMeta::new(*acc, false),
                AccountMeta::new(hotspot, false),
            ],
        );
        txs.push(env.build_transaction(&[ix]));
    }

    let sigs = submit_all_and_start(&mut env, txs).await;

    // All transactions conflict on hotspot, so must be ordered
    verify_ordered(&mut env, &sigs, STRESS_TIMEOUT, ctx).await;
}

/// Stress test: Mixed workload with independent and conflicting transactions.
/// Verifies that independent transactions can proceed while maintaining order
/// for conflicting ones.
#[tokio::test]
async fn test_replica_stress_mixed_workload() {
    let ctx = "Replica Stress Mixed Workload";
    let mut env = setup_replica_env(8);
    let count = 100;

    // Create 4 groups of accounts
    let group_a = create_accounts(&mut env, 1);
    let group_b = create_accounts(&mut env, 1);
    let independent = create_accounts(&mut env, 50);

    let mut txs = Vec::with_capacity(count);
    for i in 0..count {
        env.advance_slot();
        match i % 4 {
            0 => txs.push(tx_write(&mut env, group_a[0], i as u8)),
            1 => txs.push(tx_write(&mut env, group_b[0], i as u8)),
            _ => {
                // Independent writes to unique accounts
                let idx = (i / 4) % independent.len();
                txs.push(tx_write(&mut env, independent[idx], i as u8));
            }
        }
    }

    let sigs = submit_all_and_start(&mut env, txs).await;
    let received =
        collect_statuses(&mut env, sigs.len(), STRESS_TIMEOUT, ctx).await;

    // Extract positions of group_a and group_b transactions
    let group_a_sigs: Vec<_> = sigs
        .iter()
        .enumerate()
        .filter(|(i, _)| *i % 4 == 0)
        .map(|(_, s)| *s)
        .collect();
    let group_b_sigs: Vec<_> = sigs
        .iter()
        .enumerate()
        .filter(|(i, _)| *i % 4 == 1)
        .map(|(_, s)| *s)
        .collect();

    let group_a_positions: Vec<_> = group_a_sigs
        .iter()
        .map(|s| received.iter().position(|r| r == s).unwrap())
        .collect();
    let group_b_positions: Vec<_> = group_b_sigs
        .iter()
        .map(|s| received.iter().position(|r| r == s).unwrap())
        .collect();

    // Group A transactions must be in order
    let mut sorted_a = group_a_positions.clone();
    sorted_a.sort();
    assert_eq!(group_a_positions, sorted_a, "[{ctx}] Group A not in order");

    // Group B transactions must be in order
    let mut sorted_b = group_b_positions.clone();
    sorted_b.sort();
    assert_eq!(group_b_positions, sorted_b, "[{ctx}] Group B not in order");
}

/// Stress test: Many small transfers between pairs of accounts.
/// Each pair is independent, but transfers within a pair must be ordered.
#[tokio::test]
async fn test_replica_stress_transfer_pairs() {
    let ctx = "Replica Stress Transfer Pairs";
    let mut env = setup_replica_env(8);
    let pairs = 10;
    let transfers_per_pair = 10;
    let accs = create_accounts(&mut env, pairs * 2);

    let initial: HashMap<_, _> = accs
        .iter()
        .map(|k| (*k, env.get_account(*k).lamports()))
        .collect();

    let mut txs = Vec::with_capacity(pairs * transfers_per_pair);
    // Submit transfers for each pair in round-robin to interleave them
    for _ in 0..transfers_per_pair {
        for p in 0..pairs {
            env.advance_slot();
            let from = accs[p * 2];
            let to = accs[p * 2 + 1];
            txs.push(tx_transfer(&mut env, from, to));
        }
    }

    let sigs = submit_all_and_start(&mut env, txs).await;
    let received =
        collect_statuses(&mut env, sigs.len(), STRESS_TIMEOUT, ctx).await;

    // For each pair, verify transfers executed in order
    for p in 0..pairs {
        // Get signatures for this pair's transfers (in submission order)
        let pair_sigs: Vec<_> = (0..transfers_per_pair)
            .map(|t| sigs[t * pairs + p])
            .collect();

        // Get their execution positions
        let positions: Vec<_> = pair_sigs
            .iter()
            .map(|s| received.iter().position(|r| r == s).unwrap())
            .collect();

        // Must be in order
        let mut sorted = positions.clone();
        sorted.sort();
        assert_eq!(
            positions, sorted,
            "[{ctx}] Pair {p} transfers not in order"
        );
    }

    // Verify final balances
    let transfer_amount = 1000u64;
    for p in 0..pairs {
        let from = accs[p * 2];
        let to = accs[p * 2 + 1];
        let expected_change = transfer_amount * transfers_per_pair as u64;
        assert_eq!(
            env.get_account(from).lamports(),
            initial[&from] - expected_change,
            "[{ctx}] Pair {p} sender balance wrong"
        );
        assert_eq!(
            env.get_account(to).lamports(),
            initial[&to] + expected_change,
            "[{ctx}] Pair {p} receiver balance wrong"
        );
    }
}
