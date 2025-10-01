use super::coordinator::{ExecutionCoordinator, TransactionWithId};

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

// --- Test Setup ---

/// Creates a mock transaction with the specified accounts for testing.
fn create_mock_transaction(
    accounts: &[(Pubkey, bool)], // A tuple of (PublicKey, is_writable)
) -> TransactionWithId {
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

    let processable_txn = ProcessableTransaction {
        transaction: transaction.sanitize(false).unwrap(),
        mode: TransactionProcessingMode::Execution(None),
    };
    TransactionWithId::new(processable_txn)
}

// --- Basic Tests ---

#[test]
/// Tests that two transactions with no overlapping accounts can be scheduled concurrently.
fn test_non_conflicting_transactions() {
    let mut coordinator = ExecutionCoordinator::new(2);

    // Two transactions writing to different accounts
    let txn1 = create_mock_transaction(&[(Pubkey::new_unique(), true)]);
    let txn2 = create_mock_transaction(&[(Pubkey::new_unique(), true)]);

    let exec1 = coordinator.get_ready_executor().unwrap();
    let exec2 = coordinator.get_ready_executor().unwrap();

    // Both transactions should acquire locks without any issues.
    assert!(
        coordinator.try_acquire_locks(exec1, &txn1).is_ok(),
        "Txn1 should acquire lock without conflict"
    );
    assert!(
        coordinator.try_acquire_locks(exec2, &txn2).is_ok(),
        "Txn2 should acquire lock without conflict"
    );
}

#[test]
/// Tests that multiple transactions can take read locks on the same account concurrently.
fn test_read_read_no_contention() {
    let mut coordinator = ExecutionCoordinator::new(2);
    let shared_account = Pubkey::new_unique();

    let txn1 = create_mock_transaction(&[(shared_account, false)]);
    let txn2 = create_mock_transaction(&[(shared_account, false)]);

    let exec1 = coordinator.get_ready_executor().unwrap();
    let exec2 = coordinator.get_ready_executor().unwrap();

    // Both transactions should be able to acquire read locks on the same account.
    assert!(
        coordinator.try_acquire_locks(exec1, &txn1).is_ok(),
        "Txn1 should acquire read lock"
    );
    assert!(
        coordinator.try_acquire_locks(exec2, &txn2).is_ok(),
        "Txn2 should also acquire read lock"
    );
}

// --- Contention Tests ---

#[test]
/// Tests that a write lock blocks another write lock on the same account.
fn test_write_write_contention() {
    let mut coordinator = ExecutionCoordinator::new(2);
    let shared_account = Pubkey::new_unique();

    let txn1 = create_mock_transaction(&[(shared_account, true)]);
    let txn2 = create_mock_transaction(&[(shared_account, true)]);

    let exec1 = coordinator.get_ready_executor().unwrap();
    assert!(
        coordinator.try_acquire_locks(exec1, &txn1).is_ok(),
        "Txn1 should acquire write lock"
    );

    let exec2 = coordinator.get_ready_executor().unwrap();
    let blocker = coordinator.try_acquire_locks(exec2, &txn2).unwrap_err();

    // Txn2 should be blocked by the executor holding the lock (exec1).
    assert_eq!(blocker, exec1, "Txn2 should be blocked by executor 1");
}

#[test]
/// Tests that a write lock blocks a read lock on the same account.
fn test_write_read_contention() {
    let mut coordinator = ExecutionCoordinator::new(2);
    let shared_account = Pubkey::new_unique();

    let txn1 = create_mock_transaction(&[(shared_account, true)]);
    let txn2 = create_mock_transaction(&[(shared_account, false)]);

    let exec1 = coordinator.get_ready_executor().unwrap();
    assert!(
        coordinator.try_acquire_locks(exec1, &txn1).is_ok(),
        "Txn1 should acquire write lock"
    );

    let exec2 = coordinator.get_ready_executor().unwrap();
    let blocker = coordinator.try_acquire_locks(exec2, &txn2).unwrap_err();

    // Txn2 should be blocked by exec1.
    assert_eq!(
        blocker, exec1,
        "Read lock (Txn2) should be blocked by write lock (Txn1)"
    );
}

#[test]
/// Tests that a read lock blocks a write lock on the same account.
fn test_read_write_contention() {
    let mut coordinator = ExecutionCoordinator::new(2);
    let shared_account = Pubkey::new_unique();

    let txn1 = create_mock_transaction(&[(shared_account, false)]);
    let txn2 = create_mock_transaction(&[(shared_account, true)]);

    let exec1 = coordinator.get_ready_executor().unwrap();
    assert!(
        coordinator.try_acquire_locks(exec1, &txn1).is_ok(),
        "Txn1 should acquire read lock"
    );

    let exec2 = coordinator.get_ready_executor().unwrap();
    let blocker = coordinator.try_acquire_locks(exec2, &txn2).unwrap_err();

    // Txn2 should be blocked by exec1.
    assert_eq!(
        blocker, exec1,
        "Write lock (Txn2) should be blocked by read lock (Txn1)"
    );
}

// --- Advanced Scenarios ---

#[test]
/// Tests contention with a mix of read and write locks across multiple accounts.
fn test_multiple_mixed_locks_contention() {
    let mut coordinator = ExecutionCoordinator::new(2);

    let acc_a = Pubkey::new_unique();
    let acc_b = Pubkey::new_unique();
    let acc_c = Pubkey::new_unique();

    // Txn 1: Writes A, Reads B
    let txn1 = create_mock_transaction(&[(acc_a, true), (acc_b, false)]);
    // Txn 2: Reads A, Writes C
    let txn2 = create_mock_transaction(&[(acc_a, false), (acc_c, true)]);
    // Txn 3: Writes B, Writes C
    let txn3 = create_mock_transaction(&[(acc_b, true), (acc_c, true)]);

    let exec1 = coordinator.get_ready_executor().unwrap();
    assert!(
        coordinator.try_acquire_locks(exec1, &txn1).is_ok(),
        "Txn1 should lock A (write) and B (read)"
    );

    let exec2 = coordinator.get_ready_executor().unwrap();
    // Txn2 should be blocked by Txn1's write lock on A.
    assert_eq!(
        coordinator.try_acquire_locks(exec2, &txn2).unwrap_err(),
        exec1,
        "Txn2 should be blocked by Txn1 on account A"
    );

    // Txn3 should be blocked by Txn1's read lock on B.
    assert_eq!(
        coordinator.try_acquire_locks(exec2, &txn3).unwrap_err(),
        exec1,
        "Txn3 should be blocked by Txn1 on account B"
    );
}

#[test]
/// Tests a chain of dependencies: Txn3 waits for Txn2, which waits for Txn1.
fn test_transaction_dependency_chain() {
    let mut coordinator = ExecutionCoordinator::new(3);
    let acc_a = Pubkey::new_unique();
    let acc_b = Pubkey::new_unique();

    let txn1 = create_mock_transaction(&[(acc_a, true)]);
    let txn2 = create_mock_transaction(&[(acc_b, true), (acc_a, false)]);
    let txn3 = create_mock_transaction(&[(acc_b, false)]);

    // Schedule Txn1, which locks A for writing.
    let exec1 = coordinator.get_ready_executor().unwrap();
    assert!(coordinator.try_acquire_locks(exec1, &txn1).is_ok());

    // Txn2 needs to read A, so it's blocked by Txn1.
    let exec2 = coordinator.get_ready_executor().unwrap();
    let blocker1 = coordinator.try_acquire_locks(exec2, &txn2).unwrap_err();
    assert_eq!(blocker1, exec1, "Txn2 should be blocked by exec1");
    coordinator.queue_transaction(blocker1, txn2);

    // Txn3 needs to read B, but Txn2 (which writes to B) is already queued.
    // So, Txn3 should be blocked by Txn2's transaction ID.
    let exec3 = coordinator.get_ready_executor().unwrap();
    let blocker2 = coordinator.try_acquire_locks(exec3, &txn3).unwrap_err();
    let blocked_txn = coordinator.get_blocked_transaction(exec1).unwrap();
    assert_eq!(
        blocker2, blocked_txn.id,
        "Txn3 should be blocked by the transaction ID of Txn2"
    );
}

#[test]
/// Simulates a scenario where all executors are busy, and a new transaction gets queued and then rescheduled.
fn test_full_executor_pool_and_reschedule() {
    let mut coordinator = ExecutionCoordinator::new(2);

    let acc_a = Pubkey::new_unique();
    let acc_b = Pubkey::new_unique();
    let acc_c = Pubkey::new_unique();

    let txn1 = create_mock_transaction(&[(acc_a, true)]);
    let txn2 = create_mock_transaction(&[(acc_b, true)]);
    let txn3 = create_mock_transaction(&[(acc_a, true), (acc_c, true)]);

    // Occupy both available executors.
    let exec1 = coordinator.get_ready_executor().unwrap();
    let exec2 = coordinator.get_ready_executor().unwrap();
    assert!(coordinator.try_acquire_locks(exec1, &txn1).is_ok());
    assert!(coordinator.try_acquire_locks(exec2, &txn2).is_ok());

    // No more ready executors should be available.
    assert!(
        coordinator.get_ready_executor().is_none(),
        "Executor pool should be empty"
    );

    // Txn3 arrives and contends with Txn1 on account A.
    let blocker = coordinator.try_acquire_locks(exec1, &txn3).unwrap_err();
    assert_eq!(blocker, exec1);
    coordinator.queue_transaction(blocker, txn3);

    // Executor 1 finishes its work and releases its locks.
    coordinator.unlock_accounts(exec1);
    coordinator.release_executor(exec1);

    // Now that an executor is free, we should be able to reschedule the blocked transaction.
    let ready_exec = coordinator.get_ready_executor().unwrap();
    let blocked_txn = coordinator.get_blocked_transaction(exec1).unwrap();
    assert!(
        coordinator
            .try_acquire_locks(ready_exec, &blocked_txn)
            .is_ok(),
        "Should be able to reschedule the blocked transaction"
    );
}

// --- Edge Cases ---

#[test]
/// Tests that a transaction with no accounts can be processed without issues.
fn test_transaction_with_no_accounts() {
    let mut coordinator = ExecutionCoordinator::new(1);
    let txn = create_mock_transaction(&[]);
    let exec = coordinator.get_ready_executor().unwrap();

    assert!(
        coordinator.try_acquire_locks(exec, &txn).is_ok(),
        "Transaction with no accounts should not fail"
    );
}

#[test]
/// Tests that many read locks can be acquired on the same account concurrently.
fn test_multiple_read_locks_on_same_account() {
    let mut coordinator = ExecutionCoordinator::new(3);
    let shared_account = Pubkey::new_unique();
    let txn1 = create_mock_transaction(&[(shared_account, false)]);
    let txn2 = create_mock_transaction(&[(shared_account, false)]);
    let txn3 = create_mock_transaction(&[(shared_account, false)]);
    let exec1 = coordinator.get_ready_executor().unwrap();
    let exec2 = coordinator.get_ready_executor().unwrap();
    let exec3 = coordinator.get_ready_executor().unwrap();

    // All three should acquire read locks without contention.
    assert!(coordinator.try_acquire_locks(exec1, &txn1).is_ok());
    assert!(coordinator.try_acquire_locks(exec2, &txn2).is_ok());
    assert!(coordinator.try_acquire_locks(exec3, &txn3).is_ok());
}

#[test]
/// Tests a rapid lock-unlock-lock cycle to ensure state is managed correctly.
fn test_rapid_lock_unlock_cycle() {
    let mut coordinator = ExecutionCoordinator::new(2);
    let shared_account = Pubkey::new_unique();
    let txn1 = create_mock_transaction(&[(shared_account, true)]);
    let txn2 = create_mock_transaction(&[(shared_account, true)]);

    // Lock, unlock, and then lock again with a different transaction.
    let exec1 = coordinator.get_ready_executor().unwrap();
    assert!(coordinator.try_acquire_locks(exec1, &txn1).is_ok());
    coordinator.unlock_accounts(exec1);
    coordinator.release_executor(exec1);

    let exec2 = coordinator.get_ready_executor().unwrap();
    assert!(
        coordinator.try_acquire_locks(exec2, &txn2).is_ok(),
        "Should be able to lock the account again after it was released"
    );
}

#[test]
/// Tests rescheduling multiple transactions that were all blocked by the same executor.
fn test_reschedule_multiple_blocked_on_same_executor() {
    let mut coordinator = ExecutionCoordinator::new(2);
    let shared_account = Pubkey::new_unique();
    let txn1 = create_mock_transaction(&[(shared_account, true)]);
    let txn2 = create_mock_transaction(&[(shared_account, true)]);
    let txn3 = create_mock_transaction(&[(shared_account, true)]);

    // Txn1 takes the lock. Txn2 and Txn3 are queued as blocked.
    let exec1 = coordinator.get_ready_executor().unwrap();
    assert!(coordinator.try_acquire_locks(exec1, &txn1).is_ok());
    let exec2 = coordinator.get_ready_executor().unwrap();
    let blocker1 = coordinator.try_acquire_locks(exec2, &txn2).unwrap_err();
    coordinator.queue_transaction(blocker1, txn2);
    let blocker2 = coordinator.try_acquire_locks(exec2, &txn3).unwrap_err();
    coordinator.queue_transaction(blocker2, txn3);

    // Txn1 finishes.
    coordinator.unlock_accounts(exec1);
    coordinator.release_executor(exec1);

    // The first blocked transaction (Txn2) should now be schedulable.
    let ready_exec = coordinator.get_ready_executor().unwrap();
    let blocked_txn1 = coordinator.get_blocked_transaction(exec1).unwrap();
    let result = coordinator.try_acquire_locks(ready_exec, &blocked_txn1);
    println!("R: {result:?}");
    assert!(
        result.is_ok(),
        "First blocked transaction should be reschedulable"
    );

    // The second blocked transaction (Txn3) should still be in the queue.
    assert!(
        coordinator.get_blocked_transaction(exec1).is_some(),
        "Second blocked transaction should still be queued"
    );
}

#[test]
/// Tests a transaction that contends on multiple accounts held by different executors.
fn test_contention_on_multiple_accounts() {
    let mut coordinator = ExecutionCoordinator::new(3);
    let acc_a = Pubkey::new_unique();
    let acc_b = Pubkey::new_unique();

    let txn1 = create_mock_transaction(&[(acc_a, true)]);
    let txn2 = create_mock_transaction(&[(acc_b, true)]);
    // This transaction will contend with both Txn1 and Txn2.
    let txn3 = create_mock_transaction(&[(acc_a, true), (acc_b, true)]);

    let exec1 = coordinator.get_ready_executor().unwrap();
    let exec2 = coordinator.get_ready_executor().unwrap();
    assert!(coordinator.try_acquire_locks(exec1, &txn1).is_ok());
    assert!(coordinator.try_acquire_locks(exec2, &txn2).is_ok());

    let exec3 = coordinator.get_ready_executor().unwrap();
    // The coordinator should report the first detected contention.
    let blocker = coordinator.try_acquire_locks(exec3, &txn3).unwrap_err();
    assert_eq!(
        blocker, exec1,
        "Should be blocked by the first contended account (A)"
    );
}

#[test]
/// Tests that no ready executors are available when the pool is fully utilized.
fn test_no_ready_executors() {
    let mut coordinator = ExecutionCoordinator::new(1);
    let txn1 = create_mock_transaction(&[(Pubkey::new_unique(), true)]);
    let exec1 = coordinator.get_ready_executor().unwrap();
    assert!(coordinator.try_acquire_locks(exec1, &txn1).is_ok());

    // The only executor is now busy.
    assert!(
        coordinator.get_ready_executor().is_none(),
        "There should be no ready executors"
    );
}

#[test]
/// Tests that an executor can release locks and immediately reacquire new ones.
fn test_release_and_reacquire_lock() {
    let mut coordinator = ExecutionCoordinator::new(1);
    let acc_a = Pubkey::new_unique();
    let acc_b = Pubkey::new_unique();
    let txn1 = create_mock_transaction(&[(acc_a, true)]);
    let txn2 = create_mock_transaction(&[(acc_b, true)]);

    let exec1 = coordinator.get_ready_executor().unwrap();
    assert!(coordinator.try_acquire_locks(exec1, &txn1).is_ok());
    coordinator.unlock_accounts(exec1);

    // The executor should be able to immediately acquire a lock on a different account.
    assert!(
        coordinator.try_acquire_locks(exec1, &txn2).is_ok(),
        "Executor should be able to reacquire a lock after releasing"
    );
}

#[test]
/// Tests a scenario where a transaction is blocked by another transaction that is itself already queued.
fn test_transaction_blocked_by_queued_transaction() {
    let mut coordinator = ExecutionCoordinator::new(2);
    let shared_account = Pubkey::new_unique();

    let txn1 = create_mock_transaction(&[(shared_account, true)]);
    let txn2 = create_mock_transaction(&[(shared_account, true)]);
    let txn3 = create_mock_transaction(&[(shared_account, true)]);

    // Txn1 acquires the lock.
    let exec1 = coordinator.get_ready_executor().unwrap();
    assert!(coordinator.try_acquire_locks(exec1, &txn1).is_ok());

    // Txn2 is blocked by Txn1.
    let exec2 = coordinator.get_ready_executor().unwrap();
    let blocker1 = coordinator.try_acquire_locks(exec2, &txn2).unwrap_err();
    assert_eq!(blocker1, exec1);
    coordinator.queue_transaction(blocker1, txn2);

    // Txn3 is blocked by the already queued Txn2. The error should be the transaction ID.
    let blocker2 = coordinator.try_acquire_locks(exec2, &txn3).unwrap_err();
    let blocked_txn = coordinator.get_blocked_transaction(exec1).unwrap();
    assert_eq!(
        blocker2, blocked_txn.id,
        "Txn3 should be blocked by the ID of the queued Txn2"
    );
}
