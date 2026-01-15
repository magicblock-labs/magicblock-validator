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

use super::{
    coordinator::{ExecutionCoordinator, TransactionWithId},
    locks::{BlockerId, ExecutorId, TransactionId},
};

// --- Helpers & Setup ---

impl From<ExecutorId> for BlockerId {
    fn from(id: ExecutorId) -> Self {
        Self::Executor(id)
    }
}

impl From<TransactionId> for BlockerId {
    fn from(id: TransactionId) -> Self {
        Self::Transaction(id)
    }
}

/// Creates a mock transaction with the specified accounts.
/// Accounts: `[(Pubkey, is_writable)]`
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

    let processable = ProcessableTransaction {
        transaction: transaction.sanitize(false).unwrap(),
        mode: TransactionProcessingMode::Execution(None),
    };
    TransactionWithId::new(processable)
}

// --- Locking & Concurrency Tests ---

#[test]
fn test_non_conflicting_transactions() {
    let mut coordinator = ExecutionCoordinator::new(2);
    let txn1 = mock_txn(&[(Pubkey::new_unique(), true)]);
    let txn2 = mock_txn(&[(Pubkey::new_unique(), true)]);

    let exec1 = coordinator.get_ready_executor().unwrap();
    let exec2 = coordinator.get_ready_executor().unwrap();

    assert!(coordinator.try_acquire_locks(exec1, &txn1).is_ok());
    assert!(coordinator.try_acquire_locks(exec2, &txn2).is_ok());
}

#[test]
fn test_read_read_no_contention() {
    let mut coordinator = ExecutionCoordinator::new(2);
    let account = Pubkey::new_unique();

    let txn1 = mock_txn(&[(account, false)]);
    let txn2 = mock_txn(&[(account, false)]);

    let exec1 = coordinator.get_ready_executor().unwrap();
    let exec2 = coordinator.get_ready_executor().unwrap();

    assert!(coordinator.try_acquire_locks(exec1, &txn1).is_ok());
    assert!(coordinator.try_acquire_locks(exec2, &txn2).is_ok());
}

#[test]
fn test_write_write_contention() {
    let mut coordinator = ExecutionCoordinator::new(2);
    let account = Pubkey::new_unique();

    let txn1 = mock_txn(&[(account, true)]);
    let txn2 = mock_txn(&[(account, true)]);

    let exec1 = coordinator.get_ready_executor().unwrap();
    assert!(coordinator.try_acquire_locks(exec1, &txn1).is_ok());

    let exec2 = coordinator.get_ready_executor().unwrap();
    let blocker = coordinator.try_acquire_locks(exec2, &txn2).unwrap_err();

    assert_eq!(blocker, exec1.into());
}

#[test]
fn test_write_read_contention() {
    let mut coordinator = ExecutionCoordinator::new(2);
    let account = Pubkey::new_unique();

    let txn1 = mock_txn(&[(account, true)]); // Write
    let txn2 = mock_txn(&[(account, false)]); // Read

    let exec1 = coordinator.get_ready_executor().unwrap();
    assert!(coordinator.try_acquire_locks(exec1, &txn1).is_ok());

    let exec2 = coordinator.get_ready_executor().unwrap();
    let blocker = coordinator.try_acquire_locks(exec2, &txn2).unwrap_err();

    assert_eq!(blocker, exec1.into());
}

#[test]
fn test_read_write_contention() {
    let mut coordinator = ExecutionCoordinator::new(2);
    let account = Pubkey::new_unique();

    let txn1 = mock_txn(&[(account, false)]); // Read
    let txn2 = mock_txn(&[(account, true)]); // Write

    let exec1 = coordinator.get_ready_executor().unwrap();
    assert!(coordinator.try_acquire_locks(exec1, &txn1).is_ok());

    let exec2 = coordinator.get_ready_executor().unwrap();
    let blocker = coordinator.try_acquire_locks(exec2, &txn2).unwrap_err();

    assert_eq!(blocker, exec1.into());
}

#[test]
fn test_multiple_mixed_locks_contention() {
    let mut coordinator = ExecutionCoordinator::new(2);
    let (a, b, c) = (
        Pubkey::new_unique(),
        Pubkey::new_unique(),
        Pubkey::new_unique(),
    );

    let txn1 = mock_txn(&[(a, true), (b, false)]);
    let txn2 = mock_txn(&[(a, false), (c, true)]);
    let txn3 = mock_txn(&[(b, true), (c, true)]);

    let exec1 = coordinator.get_ready_executor().unwrap();
    assert!(coordinator.try_acquire_locks(exec1, &txn1).is_ok());

    let exec2 = coordinator.get_ready_executor().unwrap();

    // Txn2 blocked by Txn1 (Write A)
    assert_eq!(
        coordinator.try_acquire_locks(exec2, &txn2).unwrap_err(),
        exec1.into()
    );

    // Txn3 blocked by Txn1 (Read B)
    assert_eq!(
        coordinator.try_acquire_locks(exec2, &txn3).unwrap_err(),
        exec1.into()
    );
}

// --- Scheduling & Queueing Tests ---

#[test]
fn test_transaction_dependency_chain() {
    let mut coordinator = ExecutionCoordinator::new(3);
    let (a, b) = (Pubkey::new_unique(), Pubkey::new_unique());

    let txn1 = mock_txn(&[(a, true)]);
    let txn2 = mock_txn(&[(b, true), (a, false)]);
    let txn3 = mock_txn(&[(b, false)]);

    // 1. Schedule Txn1 (Locks A)
    let exec1 = coordinator.get_ready_executor().unwrap();
    assert!(coordinator.try_acquire_locks(exec1, &txn1).is_ok());

    // 2. Txn2 needs A (read), blocked by Txn1
    let exec2 = coordinator.get_ready_executor().unwrap();
    let blocker1 = coordinator.try_acquire_locks(exec2, &txn2).unwrap_err();
    assert_eq!(blocker1, exec1.into());
    coordinator.queue_transaction(blocker1, txn2);

    // 3. Txn3 needs B (read), blocked by Txn2 (which plans to write B)
    // Even though Txn2 isn't running, it's queued, so fairness blocks Txn3.
    let exec3 = coordinator.get_ready_executor().unwrap();
    let blocker2 = coordinator.try_acquire_locks(exec3, &txn3).unwrap_err();

    // Verify it is blocked by Txn2's ID
    let queued_txn2 = coordinator.next_blocked_transaction(exec1).unwrap();
    assert_eq!(blocker2, queued_txn2.id.into());
}

#[test]
fn test_full_executor_pool_and_reschedule() {
    let mut coordinator = ExecutionCoordinator::new(2);
    let (a, b, c) = (
        Pubkey::new_unique(),
        Pubkey::new_unique(),
        Pubkey::new_unique(),
    );

    let txn1 = mock_txn(&[(a, true)]);
    let txn2 = mock_txn(&[(b, true)]);
    let txn3 = mock_txn(&[(a, true), (c, true)]);

    let exec1 = coordinator.get_ready_executor().unwrap();
    let exec2 = coordinator.get_ready_executor().unwrap();

    assert!(coordinator.try_acquire_locks(exec1, &txn1).is_ok());
    assert!(coordinator.try_acquire_locks(exec2, &txn2).is_ok());
    assert!(coordinator.get_ready_executor().is_none());

    // Txn3 blocked by Txn1
    let blocker = coordinator.try_acquire_locks(exec1, &txn3).unwrap_err();
    assert_eq!(blocker, exec1.into());
    coordinator.queue_transaction(blocker, txn3);

    // Txn1 finishes
    coordinator.unlock_accounts(exec1);
    coordinator.release_executor(exec1);

    // Reschedule Txn3
    let ready_exec = coordinator.get_ready_executor().unwrap();
    let blocked_txn = coordinator.next_blocked_transaction(exec1).unwrap();
    assert!(coordinator
        .try_acquire_locks(ready_exec, &blocked_txn)
        .is_ok());
}

#[test]
fn test_reschedule_multiple_blocked_on_same_executor() {
    let mut coordinator = ExecutionCoordinator::new(2);
    let account = Pubkey::new_unique();

    let txn1 = mock_txn(&[(account, true)]);
    let txn2 = mock_txn(&[(account, true)]);
    let txn3 = mock_txn(&[(account, true)]);

    let exec1 = coordinator.get_ready_executor().unwrap();
    assert!(coordinator.try_acquire_locks(exec1, &txn1).is_ok());

    let exec2 = coordinator.get_ready_executor().unwrap();

    // Queue Txn2
    let b1 = coordinator.try_acquire_locks(exec2, &txn2).unwrap_err();
    coordinator.queue_transaction(b1, txn2);

    // Queue Txn3
    let b2 = coordinator.try_acquire_locks(exec2, &txn3).unwrap_err();
    coordinator.queue_transaction(b2, txn3);

    // Txn1 finishes
    coordinator.unlock_accounts(exec1);
    coordinator.release_executor(exec1);

    // Txn2 should be schedulable
    let ready = coordinator.get_ready_executor().unwrap();
    let t2 = coordinator.next_blocked_transaction(exec1).unwrap();
    assert!(coordinator.try_acquire_locks(ready, &t2).is_ok());

    // Txn3 remains queued
    assert!(coordinator.next_blocked_transaction(exec1).is_some());
}

#[test]
fn test_transaction_blocked_by_queued_transaction() {
    let mut coordinator = ExecutionCoordinator::new(2);
    let account = Pubkey::new_unique();

    let txn1 = mock_txn(&[(account, true)]);
    let txn2 = mock_txn(&[(account, true)]);
    let txn3 = mock_txn(&[(account, true)]);

    let exec1 = coordinator.get_ready_executor().unwrap();
    assert!(coordinator.try_acquire_locks(exec1, &txn1).is_ok());

    let exec2 = coordinator.get_ready_executor().unwrap();
    let b1 = coordinator.try_acquire_locks(exec2, &txn2).unwrap_err();
    coordinator.queue_transaction(b1, txn2);

    // Txn3 blocked by Queued Txn2
    let b2 = coordinator.try_acquire_locks(exec2, &txn3).unwrap_err();
    let queued_t2 = coordinator.next_blocked_transaction(exec1).unwrap();
    assert_eq!(b2, queued_t2.id.into());
}

// --- Edge Cases ---

#[test]
fn test_transaction_with_no_accounts() {
    let mut coordinator = ExecutionCoordinator::new(1);
    let txn = mock_txn(&[]);
    let exec = coordinator.get_ready_executor().unwrap();
    assert!(coordinator.try_acquire_locks(exec, &txn).is_ok());
}

#[test]
fn test_multiple_read_locks_on_same_account() {
    let mut coordinator = ExecutionCoordinator::new(3);
    let account = Pubkey::new_unique();

    let txn1 = mock_txn(&[(account, false)]);
    let txn2 = mock_txn(&[(account, false)]);
    let txn3 = mock_txn(&[(account, false)]);

    let e1 = coordinator.get_ready_executor().unwrap();
    let e2 = coordinator.get_ready_executor().unwrap();
    let e3 = coordinator.get_ready_executor().unwrap();

    assert!(coordinator.try_acquire_locks(e1, &txn1).is_ok());
    assert!(coordinator.try_acquire_locks(e2, &txn2).is_ok());
    assert!(coordinator.try_acquire_locks(e3, &txn3).is_ok());
}

#[test]
fn test_release_and_reacquire_lock() {
    let mut coordinator = ExecutionCoordinator::new(1);
    let (a, b) = (Pubkey::new_unique(), Pubkey::new_unique());

    let txn1 = mock_txn(&[(a, true)]);
    let txn2 = mock_txn(&[(b, true)]);

    let exec = coordinator.get_ready_executor().unwrap();
    assert!(coordinator.try_acquire_locks(exec, &txn1).is_ok());

    coordinator.unlock_accounts(exec);
    assert!(coordinator.try_acquire_locks(exec, &txn2).is_ok());
}
