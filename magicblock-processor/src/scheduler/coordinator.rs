//! Manages the state of transaction processing across multiple executors.
//!
//! This module implements the "Brain" of the scheduler. It is responsible for:
//! 1.  **Lifecycle Management:** Tracking which executors are busy or ready.
//! 2.  **Resource Locking:** orchestrating access to accounts using `AccountLock`.
//! 3.  **Fairness & Queuing:** ensuring transactions are processed in order by managing
//!     chains of dependencies (e.g., Tx B waits for Tx A, which waits for Executor 1).

use std::collections::VecDeque;

use magicblock_core::link::transactions::ProcessableTransaction;
use magicblock_metrics::metrics::MAX_LOCK_CONTENTION_QUEUE_SIZE;
use solana_pubkey::Pubkey;

use super::locks::{
    next_transaction_id, AccountContention, BlockerId, ExecutorId, LocksCache,
    RcLock, TransactionContention, TransactionId,
};

/// A queue of transactions waiting for a specific executor to release a lock.
type TransactionQueue = VecDeque<TransactionWithId>;
/// A list of transaction queues, indexed by `ExecutorId`.
type BlockedTransactionQueues = Vec<TransactionQueue>;
/// A list of locks currently held by an executor.
type AcquiredLocks = Vec<Vec<RcLock>>;

/// A wrapper that bundles a transaction with a unique, monotonic ID.
/// This ID is critical for enforcing First-In-First-Out (FIFO) fairness.
pub(super) struct TransactionWithId {
    pub(super) id: TransactionId,
    pub(super) txn: ProcessableTransaction,
}

impl TransactionWithId {
    pub(super) fn new(txn: ProcessableTransaction) -> Self {
        let id = next_transaction_id();
        Self { id, txn }
    }
}

/// The central state machine for the scheduler.
pub(super) struct ExecutionCoordinator {
    /// Transactions waiting for a specific executor to finish.
    blocked_transactions: BlockedTransactionQueues,
    /// Maps a blocked transaction ID to the Executor ID blocking it.
    transaction_contention: TransactionContention,
    /// Maps an account to a list of transactions waiting to access it (ordered by ID).
    account_contention: AccountContention,
    /// Pool of idle executors ready for work.
    ready_executors: Vec<ExecutorId>,
    /// Locks currently held by each executor.
    acquired_locks: AcquiredLocks,
    /// The global registry of all account locks.
    locks: LocksCache,
}

impl ExecutionCoordinator {
    pub(super) fn new(count: usize) -> Self {
        Self {
            blocked_transactions: (0..count).map(|_| VecDeque::new()).collect(),
            acquired_locks: (0..count).map(|_| Vec::new()).collect(),
            ready_executors: (0..count as u32).collect(),
            transaction_contention: TransactionContention::default(),
            account_contention: AccountContention::default(),
            locks: LocksCache::default(),
        }
    }

    // --- Readiness Management ---

    /// Returns true if at least one executor is idle.
    #[inline]
    pub(super) fn is_ready(&self) -> bool {
        !self.ready_executors.is_empty()
    }

    /// Pops a ready executor from the pool.
    #[inline]
    pub(super) fn get_ready_executor(&mut self) -> Option<ExecutorId> {
        self.ready_executors.pop()
    }

    /// Returns an executor to the ready pool.
    #[inline]
    pub(super) fn release_executor(&mut self, executor: ExecutorId) {
        self.ready_executors.push(executor)
    }

    // --- Locking & Scheduling ---

    /// Attempts to schedule a transaction on the given executor.
    ///
    /// This is an all-or-nothing operation. If *any* lock cannot be acquired (due to
    /// an active writer or a fairness conflict), the entire transaction is rejected,
    /// all partial locks are released, and the transaction is queued behind the blocker.
    ///
    /// Returns:
    /// - `Ok(txn)`: Locks acquired, ready for execution.
    /// - `Err(blocking_executor)`: Scheduling failed; `executor` was returned to the pool.
    ///   The returned ID is the executor causing the block (used for rescheduling hints).
    pub(super) fn try_schedule(
        &mut self,
        executor: ExecutorId,
        txn: TransactionWithId,
    ) -> Result<ProcessableTransaction, ExecutorId> {
        match self.try_acquire_locks(executor, &txn) {
            Ok(()) => Ok(txn.txn),
            Err(blocker) => {
                // We failed to lock, so this executor is free to try another task.
                self.release_executor(executor);
                // Queue the transaction behind the entity blocking it.
                let blocking_executor = self.queue_transaction(blocker, txn);
                Err(blocking_executor)
            }
        }
    }

    /// Helper that orchestrates the locking attempt, commit, and rollback phases.
    pub(crate) fn try_acquire_locks(
        &mut self,
        executor: ExecutorId,
        txn: &TransactionWithId,
    ) -> Result<(), BlockerId> {
        match self.lock_accounts(executor, txn) {
            Ok(()) => {
                self.cleanup_success(txn);
                Ok(())
            }
            Err(blocker) => {
                self.handle_failure(executor, txn);
                Err(blocker)
            }
        }
    }

    /// Iterates through the transaction's accounts and attempts to acquire locks.
    fn lock_accounts(
        &mut self,
        executor: ExecutorId,
        txn: &TransactionWithId,
    ) -> Result<(), BlockerId> {
        let message = txn.txn.transaction.message();
        let accounts = message.account_keys();

        for (i, &acc) in accounts.iter().enumerate() {
            // 1. Fairness Check:
            // Before touching the lock, check if an older transaction is already waiting
            // for this account. If so, we must wait our turn.
            self.check_contention(acc, txn.id)?;

            // 2. Lock Acquisition:
            let lock = self.locks.entry(acc).or_default().clone();
            let mut guard = lock.borrow_mut();

            let result = if message.is_writable(i) {
                guard.write(executor)
            } else {
                guard.read(executor)
            };

            match result {
                Ok(()) => {
                    self.acquired_locks[executor as usize].push(lock.clone())
                }
                Err(holder) => return Err(BlockerId::Executor(holder)),
            }
        }
        Ok(())
    }

    /// Releases all locks held by a specific executor.
    /// Called when an executor finishes a transaction or when a scheduling attempt fails.
    pub(crate) fn unlock_accounts(&mut self, executor: ExecutorId) {
        let locks = &mut self.acquired_locks[executor as usize];
        while let Some(lock) = locks.pop() {
            lock.borrow_mut().unlock(executor);
        }
    }

    // --- Contention & Queueing ---

    /// Queues a transaction behind the executor that is blocking it.
    ///
    /// If the blocker is another *Transaction*, we resolve it to the underlying *Executor*
    /// currently blocking that transaction.
    pub(super) fn queue_transaction(
        &mut self,
        blocker: BlockerId,
        txn: TransactionWithId,
    ) -> ExecutorId {
        let executor = match blocker {
            BlockerId::Executor(e) => e,
            BlockerId::Transaction(tx_id) => *self
                .transaction_contention
                .get(&tx_id)
                .expect("Critical: Blocker transaction must exist in contention map"),
        };

        // Record that `txn` is blocked by `executor`.
        self.transaction_contention.insert(txn.id, executor);

        // Insert into the executor's wait queue, maintaining ID order.
        let queue = &mut self.blocked_transactions[executor as usize];
        let idx = queue
            .binary_search_by_key(&txn.id, |t| t.id)
            .unwrap_or_else(|i| i);
        queue.insert(idx, txn);

        let current_max = MAX_LOCK_CONTENTION_QUEUE_SIZE.get();
        MAX_LOCK_CONTENTION_QUEUE_SIZE.set(current_max.max(queue.len() as i64));

        executor
    }

    /// Retrieves the next transaction waiting for this executor.
    pub(super) fn next_blocked_transaction(
        &mut self,
        executor: ExecutorId,
    ) -> Option<TransactionWithId> {
        self.blocked_transactions[executor as usize].pop_front()
    }

    // --- Helpers ---

    /// Verifies if the current transaction is allowed to lock `acc`.
    /// Returns `Err(BlockerId)` if an earlier transaction is already queued for this account.
    fn check_contention(
        &self,
        acc: Pubkey,
        txn_id: TransactionId,
    ) -> Result<(), BlockerId> {
        if let Some(contenders) = self.account_contention.get(&acc) {
            match contenders.binary_search(&txn_id) {
                // If we are in the list but NOT at the front (index > 0),
                // we are blocked by the transaction immediately preceding us.
                Ok(i) | Err(i) if i > 0 => {
                    return Err(BlockerId::Transaction(contenders[i - 1]));
                }
                // If we are at index 0 (front of line) or not in the list (Err 0),
                // we are free to proceed.
                _ => {}
            }
        }
        Ok(())
    }

    /// Cleans up contention metadata after a successful schedule.
    /// Removes the transaction from all "waiting lists".
    fn cleanup_success(&mut self, txn: &TransactionWithId) {
        self.transaction_contention.remove(&txn.id);

        let message = txn.txn.transaction.message();
        for acc in message.account_keys().iter() {
            if let Some(contenders) = self.account_contention.get_mut(acc) {
                if let Ok(i) = contenders.binary_search(&txn.id) {
                    contenders.remove(i);
                }
                if contenders.is_empty() {
                    self.account_contention.remove(acc);
                }
            }
        }
    }

    /// Handles a failed locking attempt (Rollback).
    /// 1. Releases any locks partially acquired during this attempt.
    /// 2. Marks writable accounts as "contended" by this transaction to reserve a spot in line.
    fn handle_failure(
        &mut self,
        executor: ExecutorId,
        txn: &TransactionWithId,
    ) {
        self.unlock_accounts(executor);

        // Register contention for writable accounts.
        // This ensures that even if we failed to lock now, we "get in line" so that
        // future transactions don't jump ahead of us, preventing starvation.
        let message = txn.txn.transaction.message();
        for (i, &acc) in message.account_keys().iter().enumerate() {
            if message.is_writable(i) {
                let contenders =
                    self.account_contention.entry(acc).or_default();
                if let Err(i) = contenders.binary_search(&txn.id) {
                    contenders.insert(i, txn.id);
                }
            }
        }
    }
}
