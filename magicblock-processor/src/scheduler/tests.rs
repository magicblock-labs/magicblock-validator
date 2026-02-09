//! Unit tests for the scheduler's locking and queueing logic.
//!
//! Tests cover:
//! - Lock semantics (write/write, write/read, read/write contention)
//! - FIFO ordering within blocked queues
//! - Executor pool management
//! - Edge cases (empty transactions, duplicate accounts)

use magicblock_core::link::transactions::{
    ProcessableTransaction, SanitizeableTransaction, TransactionProcessingMode,
};
use solana_keypair::Keypair;
use solana_program::{
    hash::Hash,
    instruction::{AccountMeta, Instruction},
};
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use solana_transaction::Transaction;

use super::coordinator::{ExecutionCoordinator, TransactionWithId};

/// Creates a mock transaction with the specified accounts.
///
/// # Arguments
///
/// * `accounts` - Slice of `(Pubkey, is_writable)` tuples
fn mock_txn(accounts: &[(Pubkey, bool)]) -> TransactionWithId {
    let payer = Keypair::new();
    let instructions: Vec<Instruction> = accounts
        .iter()
        .map(|(pubkey, is_writable)| {
            let meta = if *is_writable {
                AccountMeta::new(*pubkey, false)
            } else {
                AccountMeta::new_readonly(*pubkey, false)
            };
            Instruction::new_with_bincode(Pubkey::new_unique(), &(), vec![meta])
        })
        .collect();

    let transaction = Transaction::new_signed_with_payer(
        &instructions,
        Some(&payer.pubkey()),
        &[payer],
        Hash::new_unique(),
    );

    TransactionWithId::new(ProcessableTransaction {
        transaction: transaction.sanitize(false).unwrap(),
        mode: TransactionProcessingMode::Execution(None),
    })
}

// =============================================================================
// Lock Semantics
// =============================================================================

#[test]
fn write_blocks_write() {
    let mut c = ExecutionCoordinator::new(2);
    let acc = Pubkey::new_unique();

    let e0 = c.get_ready_executor().unwrap();
    let e1 = c.get_ready_executor().unwrap();

    assert!(c.try_schedule(e0, mock_txn(&[(acc, true)])).is_ok());
    assert_eq!(c.try_schedule(e1, mock_txn(&[(acc, true)])).err(), Some(e0));
}

#[test]
fn write_blocks_read() {
    let mut c = ExecutionCoordinator::new(2);
    let acc = Pubkey::new_unique();

    let e0 = c.get_ready_executor().unwrap();
    let e1 = c.get_ready_executor().unwrap();

    assert!(c.try_schedule(e0, mock_txn(&[(acc, true)])).is_ok());
    assert_eq!(
        c.try_schedule(e1, mock_txn(&[(acc, false)])).err(),
        Some(e0)
    );
}

#[test]
fn read_blocks_write() {
    let mut c = ExecutionCoordinator::new(2);
    let acc = Pubkey::new_unique();

    let e0 = c.get_ready_executor().unwrap();
    let e1 = c.get_ready_executor().unwrap();

    assert!(c.try_schedule(e0, mock_txn(&[(acc, false)])).is_ok());
    assert_eq!(c.try_schedule(e1, mock_txn(&[(acc, true)])).err(), Some(e0));
}

#[test]
fn multiple_readers_allowed() {
    let mut c = ExecutionCoordinator::new(3);
    let acc = Pubkey::new_unique();

    let e0 = c.get_ready_executor().unwrap();
    let e1 = c.get_ready_executor().unwrap();
    let e2 = c.get_ready_executor().unwrap();

    assert!(c.try_schedule(e0, mock_txn(&[(acc, false)])).is_ok());
    assert!(c.try_schedule(e1, mock_txn(&[(acc, false)])).is_ok());
    assert!(c.try_schedule(e2, mock_txn(&[(acc, false)])).is_ok());
}

// =============================================================================
// Partial Lock Rollback
// =============================================================================

#[test]
fn partial_locks_released_on_failure() {
    let mut c = ExecutionCoordinator::new(2);
    let (a, b) = (Pubkey::new_unique(), Pubkey::new_unique());

    let e0 = c.get_ready_executor().unwrap();
    let e1 = c.get_ready_executor().unwrap();

    // e0 locks B only
    assert!(c.try_schedule(e0, mock_txn(&[(b, true)])).is_ok());

    // e1 tries [A, B] - locks A, fails on B, should release A
    assert_eq!(
        c.try_schedule(e1, mock_txn(&[(a, true), (b, true)])).err(),
        Some(e0)
    );

    // If A wasn't released, this would fail
    let e2 = c.get_ready_executor().unwrap();
    assert!(c.try_schedule(e2, mock_txn(&[(a, true)])).is_ok());
}

// =============================================================================
// Queue Ordering (FIFO)
// =============================================================================

#[test]
fn blocked_transactions_dequeued_in_fifo_order() {
    let mut c = ExecutionCoordinator::new(4);
    let acc = Pubkey::new_unique();

    let e0 = c.get_ready_executor().unwrap();
    assert!(c.try_schedule(e0, mock_txn(&[(acc, true)])).is_ok());

    // Queue 3 transactions - they get IDs in order
    let t1 = mock_txn(&[(acc, true)]);
    let t2 = mock_txn(&[(acc, true)]);
    let t3 = mock_txn(&[(acc, true)]);
    let id1 = t1.id;
    let id2 = t2.id;
    let id3 = t3.id;
    assert!(id1 < id2 && id2 < id3);

    // Queue in reverse order to test heap ordering
    let e = c.get_ready_executor().unwrap();
    assert!(c.try_schedule(e, t3).is_err());
    let e = c.get_ready_executor().unwrap();
    assert!(c.try_schedule(e, t1).is_err());
    let e = c.get_ready_executor().unwrap();
    assert!(c.try_schedule(e, t2).is_err());

    // Should dequeue in ID order (FIFO), not insertion order
    let d1 = c.next_blocked_transaction(e0).unwrap();
    let d2 = c.next_blocked_transaction(e0).unwrap();
    let d3 = c.next_blocked_transaction(e0).unwrap();

    assert_eq!(d1.id, id1);
    assert_eq!(d2.id, id2);
    assert_eq!(d3.id, id3);
}

// =============================================================================
// Executor Pool Management
// =============================================================================

#[test]
fn blocked_transaction_releases_executor() {
    let mut c = ExecutionCoordinator::new(2);
    let acc = Pubkey::new_unique();

    let e0 = c.get_ready_executor().unwrap();
    let e1 = c.get_ready_executor().unwrap();
    assert!(c.get_ready_executor().is_none()); // Pool exhausted

    assert!(c.try_schedule(e0, mock_txn(&[(acc, true)])).is_ok());
    assert!(c.try_schedule(e1, mock_txn(&[(acc, true)])).is_err());

    // e1 should be back in pool after being blocked
    assert!(c.get_ready_executor().is_some());
}

// =============================================================================
// Unlock and Reschedule
// =============================================================================

#[test]
fn unlock_allows_blocked_transaction_to_proceed() {
    let mut c = ExecutionCoordinator::new(2);
    let acc = Pubkey::new_unique();

    let e0 = c.get_ready_executor().unwrap();
    let e1 = c.get_ready_executor().unwrap();

    assert!(c.try_schedule(e0, mock_txn(&[(acc, true)])).is_ok());
    assert!(c.try_schedule(e1, mock_txn(&[(acc, true)])).is_err());

    // Unlock e0's locks
    c.unlock_accounts(e0);

    // Now blocked txn should be able to acquire locks
    let blocked = c.next_blocked_transaction(e0).unwrap();
    let e = c.get_ready_executor().unwrap();
    assert!(c.try_schedule(e, blocked).is_ok());
}

#[test]
fn transaction_requeued_to_different_executor_keeps_id() {
    let mut c = ExecutionCoordinator::new(3);
    let (a, b) = (Pubkey::new_unique(), Pubkey::new_unique());

    let e0 = c.get_ready_executor().unwrap();
    let e1 = c.get_ready_executor().unwrap();
    let e2 = c.get_ready_executor().unwrap();

    // e0 holds A, e1 holds B
    assert!(c.try_schedule(e0, mock_txn(&[(a, true)])).is_ok());
    assert!(c.try_schedule(e1, mock_txn(&[(b, true)])).is_ok());

    // t1 needs [A, B] - blocked by e0 on A
    let t1 = mock_txn(&[(a, true), (b, true)]);
    let original_id = t1.id;
    assert_eq!(c.try_schedule(e2, t1).err(), Some(e0));

    // Unlock e0, t1 retries but now blocked by e1 on B
    c.unlock_accounts(e0);
    let t1 = c.next_blocked_transaction(e0).unwrap();
    assert_eq!(t1.id, original_id); // Same ID

    let e = c.get_ready_executor().unwrap();
    assert_eq!(c.try_schedule(e, t1).err(), Some(e1));

    // Verify it's in e1's queue with same ID
    let t1 = c.next_blocked_transaction(e1).unwrap();
    assert_eq!(t1.id, original_id);
}

// =============================================================================
// Edge Cases
// =============================================================================

#[test]
fn empty_transaction_always_succeeds() {
    let mut c = ExecutionCoordinator::new(1);
    let e = c.get_ready_executor().unwrap();
    assert!(c.try_schedule(e, mock_txn(&[])).is_ok());
}

#[test]
fn transaction_with_duplicate_accounts() {
    // Real transactions shouldn't have duplicates, but verify we don't panic
    let mut c = ExecutionCoordinator::new(1);
    let acc = Pubkey::new_unique();
    let e = c.get_ready_executor().unwrap();

    // Same account twice as writable - second lock attempt is no-op (already held)
    assert!(c
        .try_schedule(e, mock_txn(&[(acc, true), (acc, true)]))
        .is_ok());
}
