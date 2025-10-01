//! Manages the state of transaction processing across multiple executors.
//!
//! This module contains the `ExecutionCoordinator`, which tracks ready executors,
//! queues of blocked transactions, and the locks held by each worker. It acts
//! as the central state machine for the scheduling process.

use std::collections::VecDeque;

use magicblock_core::link::transactions::ProcessableTransaction;

use super::locks::{
    next_transaction_id, ExecutorId, LocksCache, RcLock, TransactionContention,
    TransactionId, MAX_SVM_EXECUTORS,
};

// --- Type Aliases ---

/// A queue of transactions waiting for a specific executor to release a lock.
type TransactionQueue = VecDeque<TransactionWithId>;
/// A list of transaction queues, indexed by `ExecutorId`. Each executor has its own queue.
type BlockedTransactionQueues = Vec<TransactionQueue>;
/// A list of all locks acquired by an executor, indexed by `ExecutorId`.
type AcquiredLocks = Vec<Vec<RcLock>>;

/// A transaction bundled with its unique ID for tracking purposes.
pub(super) struct TransactionWithId {
    pub(super) id: TransactionId,
    pub(super) txn: ProcessableTransaction,
}

impl TransactionWithId {
    /// Creates a new transaction with a unique ID.
    pub(super) fn new(txn: ProcessableTransaction) -> Self {
        Self {
            id: next_transaction_id(),
            txn,
        }
    }
}

/// Manages the state for all transaction executors, including their
/// readiness, blocked transactions, and acquired account locks.
pub(super) struct ExecutionCoordinator {
    /// A queue for each executor to hold transactions that are waiting for its locks.
    blocked_transactions: BlockedTransactionQueues,
    /// A map tracking which executor is blocking which transaction.
    transaction_contention: TransactionContention,
    /// A pool of executor IDs that are currently idle and ready for new work.
    ready_executors: Vec<ExecutorId>,
    /// A list of locks currently held by each executor.
    acquired_locks: AcquiredLocks,
    /// The global cache of all account locks.
    locks: LocksCache,
}

impl ExecutionCoordinator {
    /// Creates a new `ExecutionCoordinator` for a given number of executors.
    pub(super) fn new(count: usize) -> Self {
        Self {
            blocked_transactions: (0..count).map(|_| VecDeque::new()).collect(),
            acquired_locks: (0..count).map(|_| Vec::new()).collect(),
            ready_executors: (0..count as u32).collect(),
            transaction_contention: TransactionContention::default(),
            locks: LocksCache::default(),
        }
    }

    /// Queues a transaction that is blocked by a contended lock.
    ///
    /// The `blocker_id` can be either an `ExecutorId` or a `TransactionId`.
    /// If it's a `TransactionId`, this function resolves it to the underlying
    /// `ExecutorId` that holds the conflicting lock.
    pub(super) fn queue_transaction(
        &mut self,
        mut blocker_id: u32,
        transaction: TransactionWithId,
    ) {
        // A `blocker_id` greater than `MAX_SVM_EXECUTORS` is a `TransactionId`
        // of another waiting transaction. We must resolve it to the actual executor.
        if blocker_id >= MAX_SVM_EXECUTORS {
            // SAFETY: This unwrap is safe. A `TransactionId` is only returned as a
            // blocker if that transaction is already tracked in the contention map.
            blocker_id = self
                .transaction_contention
                .get(&blocker_id)
                .copied()
                .unwrap_or(ExecutorId::MIN);
        }

        let queue = &mut self.blocked_transactions[blocker_id as usize];
        self.transaction_contention
            .insert(transaction.id, blocker_id);
        queue.push_back(transaction);
    }

    /// Checks if there are any executors ready to process a transaction.
    pub(super) fn is_ready(&self) -> bool {
        !self.ready_executors.is_empty()
    }

    /// Retrieves the ID of a ready executor, if one is available.
    pub(super) fn get_ready_executor(&mut self) -> Option<ExecutorId> {
        self.ready_executors.pop()
    }

    /// Returns an executor to the pool of ready executors.
    pub(super) fn release_executor(&mut self, executor: ExecutorId) {
        self.ready_executors.push(executor)
    }

    /// Releases all account locks held by a specific executor.
    pub(crate) fn unlock_accounts(&mut self, executor: ExecutorId) {
        let locks = &mut self.acquired_locks[executor as usize];
        // Iteratively drain the list of acquired locks.
        while let Some(lock) = locks.pop() {
            lock.borrow_mut().unlock(executor);
        }
    }

    /// Retrieves the next blocked transaction waiting for a given executor.
    pub(super) fn get_blocked_transaction(
        &mut self,
        executor: ExecutorId,
    ) -> Option<TransactionWithId> {
        self.blocked_transactions[executor as usize].pop_front()
    }

    /// Attempts to acquire all necessary read and write locks for a transaction.
    ///
    /// This function iterates through all accounts in the transaction's message and
    /// attempts to acquire the appropriate lock for each. If any lock is contended,
    /// it fails early and returns the ID of the blocking executor or transaction.
    pub(super) fn try_acquire_locks(
        &mut self,
        executor: ExecutorId,
        transaction: &TransactionWithId,
    ) -> Result<(), u32> {
        let message = transaction.txn.transaction.message();
        let accounts_to_lock = message.account_keys().iter().enumerate();
        let acquired_locks = &mut self.acquired_locks[executor as usize];

        for (i, &acc) in accounts_to_lock {
            // Get or create the lock for the account.
            let lock = self.locks.entry(acc).or_default().clone();

            // Attempt to acquire a write or read lock.
            let result = if message.is_writable(i) {
                lock.borrow_mut().write(executor, transaction.id)
            } else {
                lock.borrow_mut().read(executor, transaction.id)
            };
            acquired_locks.push(lock);

            if result.is_err() {
                for lock in acquired_locks.drain(..) {
                    let mut lock = lock.borrow_mut();
                    lock.unlock_with_contention(executor, transaction.id);
                }
            }
            result?;
        }

        // On success, the transaction is no longer blocking anything.
        self.transaction_contention.remove(&transaction.id);
        Ok(())
    }
}
