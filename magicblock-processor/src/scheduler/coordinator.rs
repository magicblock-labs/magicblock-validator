//! Manages the state of transaction processing across multiple executors.
//!
//! This module contains the `ExecutionCoordinator`, which tracks ready executors,
//! queues of blocked transactions, and the locks held by each worker.

use std::collections::VecDeque;

use magicblock_core::link::transactions::ProcessableTransaction;

use super::locks::{ExecutorId, LocksCache, RcLock};

/// A queue of transactions waiting to be processed by a specific executor.
type TransactionQueue = VecDeque<ProcessableTransaction>;
/// A list of transaction queues, indexed by `ExecutorId`.
type BlockedTransactionQueues = Vec<TransactionQueue>;
/// A list of all locks acquired by an executor, indexed by `ExecutorId`.
type AcquiredLocks = Vec<Vec<RcLock>>;

/// Manages the state for all transaction executors, including their
/// readiness, blocked transactions, and acquired account locks.
pub(super) struct ExecutionCoordinator {
    /// A queue for each executor to hold transactions that are waiting for locks.
    blocked_transactions: BlockedTransactionQueues,
    /// A pool of executor IDs that are currently idle and ready for new work.
    ready_executors: Vec<ExecutorId>,
    /// A list of locks currently held by each executor.
    acquired_locks: AcquiredLocks,
    /// The cache of all account locks.
    locks: LocksCache,
}

impl ExecutionCoordinator {
    /// Creates a new `ExecutionCoordinator` for a given number of executors.
    pub(super) fn new(count: usize) -> Self {
        Self {
            blocked_transactions: (0..count).map(|_| VecDeque::new()).collect(),
            acquired_locks: (0..count).map(|_| Vec::new()).collect(),
            ready_executors: (0..count as u32).collect(),
            locks: LocksCache::default(),
        }
    }

    /// Queues a transaction to be processed by a specific executor once its
    /// required locks are available.
    pub(super) fn queue_transaction(
        &mut self,
        executor: ExecutorId,
        transaction: ProcessableTransaction,
    ) {
        let queue = &mut self.blocked_transactions[executor as usize];
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
        while let Some(lock) = locks.pop() {
            lock.borrow_mut().unlock(executor);
        }
    }

    /// Retrieves the next blocked transaction for a given executor.
    pub(super) fn get_blocked_transaction(
        &mut self,
        executor: ExecutorId,
    ) -> Option<ProcessableTransaction> {
        self.blocked_transactions[executor as usize].pop_front()
    }

    /// Attempts to acquire all necessary read and write locks for a transaction.
    ///
    /// If any lock is contended, this function will fail and return the ID of the
    /// executor that holds the conflicting lock.
    pub(super) fn try_acquire_locks(
        &mut self,
        executor: ExecutorId,
        txn: &ProcessableTransaction,
    ) -> Result<(), ExecutorId> {
        let message = txn.transaction.message();
        let accounts_to_lock = message.account_keys().iter().enumerate();

        for (i, &acc) in accounts_to_lock {
            let lock = self.locks.entry(acc).or_default().clone();
            if message.is_writable(i) {
                lock.borrow_mut().write(executor)?;
            } else {
                lock.borrow_mut().read(executor)?;
            };

            self.acquired_locks[executor as usize].push(lock);
        }
        Ok(())
    }
}
