//! Manages the state of transaction processing across multiple executors.
//!
//! This module contains the `ExecutionCoordinator`, which tracks ready executors,
//! queues of blocked transactions, and the locks held by each worker.

use std::collections::VecDeque;

use magicblock_core::link::transactions::ProcessableTransaction;

use super::locks::{
    next_transaction_id, ExecutorId, LocksCache, RcLock, TransactionId,
    TransactionQueues, MAX_SVM_EXECUTORS,
};

/// A queue of transactions waiting to be processed by a specific executor.
type TransactionQueue = VecDeque<TransactionWithId>;
/// A list of transaction queues, indexed by `ExecutorId`.
type BlockedTransactionQueues = Vec<TransactionQueue>;
/// A list of all locks acquired by an executor, indexed by `ExecutorId`.
type AcquiredLocks = Vec<Vec<RcLock>>;

pub(super) struct TransactionWithId {
    pub(super) id: TransactionId,
    pub(super) txn: ProcessableTransaction,
}

impl TransactionWithId {
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
    /// A queue for each executor to hold transactions that are waiting for locks.
    blocked_transactions: BlockedTransactionQueues,
    transaction_queues: TransactionQueues,
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
            transaction_queues: TransactionQueues::default(),
            locks: LocksCache::default(),
        }
    }

    /// Queues a transaction to be processed by a specific executor once its
    /// required locks are available.
    pub(super) fn queue_transaction(
        &mut self,
        mut blocker: u32,
        transaction: TransactionWithId,
    ) {
        if blocker >= MAX_SVM_EXECUTORS {
            // unwrap will never happen, as every pending transaction (which is a contender/blocker) will have an associated entry in the hashmap
            blocker = self
                .transaction_queues
                .get(&blocker)
                .copied()
                .unwrap_or(ExecutorId::MIN);
        }
        let queue = &mut self.blocked_transactions[blocker as usize];
        self.transaction_queues.insert(transaction.id, blocker);
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
    ) -> Option<TransactionWithId> {
        self.blocked_transactions[executor as usize].pop_front()
    }

    /// Attempts to acquire all necessary read and write locks for a transaction.
    ///
    /// If any lock is contended, this function will fail and return the ID of the
    /// executor that holds the conflicting lock.
    pub(super) fn try_acquire_locks(
        &mut self,
        executor: ExecutorId,
        transaction: &TransactionWithId,
    ) -> Result<(), ExecutorId> {
        let message = transaction.txn.transaction.message();
        let accounts_to_lock = message.account_keys().iter().enumerate();

        for (i, &acc) in accounts_to_lock {
            let lock = self.locks.entry(acc).or_default().clone();
            if message.is_writable(i) {
                lock.borrow_mut().write(executor, transaction.id)?;
            } else {
                lock.borrow_mut().read(executor, transaction.id)?;
            };

            self.acquired_locks[executor as usize].push(lock);
        }
        Ok(())
    }
}
