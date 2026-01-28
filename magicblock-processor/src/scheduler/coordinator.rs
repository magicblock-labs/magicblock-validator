//! Manages transaction scheduling across multiple executors.
//!
//! Simple locking strategy: try to acquire all locks, if any fails,
//! release partial locks and queue behind the blocking executor.
//! No fairness guarantees - livelocks are acceptable.

use std::collections::VecDeque;

use magicblock_core::link::transactions::ProcessableTransaction;
use magicblock_metrics::metrics::MAX_LOCK_CONTENTION_QUEUE_SIZE;

use super::locks::{ExecutorId, LocksCache, RcLock};

type TransactionQueue = VecDeque<ProcessableTransaction>;

/// The central state machine for the scheduler.
pub(super) struct ExecutionCoordinator {
    /// Transactions waiting for a specific executor to finish.
    blocked_transactions: Vec<TransactionQueue>,
    /// Pool of idle executors ready for work.
    ready_executors: Vec<ExecutorId>,
    /// Locks currently held by each executor.
    acquired_locks: Vec<Vec<RcLock>>,
    /// The global registry of all account locks.
    locks: LocksCache,
}

impl ExecutionCoordinator {
    pub(super) fn new(count: usize) -> Self {
        Self {
            blocked_transactions: (0..count).map(|_| VecDeque::new()).collect(),
            acquired_locks: (0..count).map(|_| Vec::new()).collect(),
            ready_executors: (0..count as u32).collect(),
            locks: LocksCache::default(),
        }
    }

    #[inline]
    pub(super) fn is_ready(&self) -> bool {
        !self.ready_executors.is_empty()
    }

    #[inline]
    pub(super) fn get_ready_executor(&mut self) -> Option<ExecutorId> {
        self.ready_executors.pop()
    }

    #[inline]
    pub(super) fn release_executor(&mut self, executor: ExecutorId) {
        self.ready_executors.push(executor)
    }

    /// Attempts to schedule a transaction on the given executor.
    /// Returns Ok(txn) if locks acquired, Err(blocking_executor) if blocked.
    pub(super) fn try_schedule(
        &mut self,
        executor: ExecutorId,
        txn: ProcessableTransaction,
    ) -> Result<ProcessableTransaction, ExecutorId> {
        match self.try_acquire_locks(executor, &txn) {
            Ok(()) => Ok(txn),
            Err(blocker) => {
                self.release_executor(executor);
                self.queue_transaction(blocker, txn);
                Err(blocker)
            }
        }
    }

    /// Try to acquire all locks for the transaction.
    pub(crate) fn try_acquire_locks(
        &mut self,
        executor: ExecutorId,
        txn: &ProcessableTransaction,
    ) -> Result<(), ExecutorId> {
        let message = txn.transaction.message();
        let accounts = message.account_keys();

        for (i, &acc) in accounts.iter().enumerate() {
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
                Err(blocker) => {
                    self.unlock_accounts(executor);
                    return Err(blocker);
                }
            }
        }
        Ok(())
    }

    /// Releases all locks held by a specific executor.
    pub(crate) fn unlock_accounts(&mut self, executor: ExecutorId) {
        let locks = &mut self.acquired_locks[executor as usize];
        while let Some(lock) = locks.pop() {
            lock.borrow_mut().unlock(executor);
        }
    }

    /// Queues a transaction behind the blocking executor.
    fn queue_transaction(
        &mut self,
        blocker: ExecutorId,
        txn: ProcessableTransaction,
    ) {
        let queue = &mut self.blocked_transactions[blocker as usize];
        queue.push_back(txn);

        let current_max = MAX_LOCK_CONTENTION_QUEUE_SIZE.get();
        MAX_LOCK_CONTENTION_QUEUE_SIZE.set(current_max.max(queue.len() as i64));
    }

    /// Retrieves the next transaction waiting for this executor.
    pub(super) fn next_blocked_transaction(
        &mut self,
        executor: ExecutorId,
    ) -> Option<ProcessableTransaction> {
        self.blocked_transactions[executor as usize].pop_front()
    }
}
