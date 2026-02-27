//! Transaction scheduling coordination across multiple executors.
//!
//! # Architecture
//!
//! - **Lock acquisition**: All-or-nothing attempt on all transaction accounts
//! - **Lock contention**: Failed transactions queued behind blocking executor
//! - **FIFO ordering**: Transactions processed in ID order within each blocked queue
//!
//! # Key Types
//!
//! - `ExecutorId`: Unique identifier for each executor worker (0..N)
//! - `TransactionId`: Monotonic ID for FIFO queue ordering
//! - `BinaryHeap<TransactionWithId>`: Min-heap ordered by transaction ID

use std::{cmp::Ordering, collections::BinaryHeap};

use magicblock_core::link::transactions::ProcessableTransaction;
use magicblock_metrics::metrics::MAX_LOCK_CONTENTION_QUEUE_SIZE;

use super::locks::{
    next_transaction_id, ExecutorId, LocksCache, TransactionId,
};
use crate::scheduler::locks::RcLock;

/// Coordinates transaction scheduling across multiple executor workers.
///
/// # Scheduling Flow
///
/// 1. Acquire idle executor from pool
/// 2. Try to lock all transaction accounts (all-or-nothing)
/// 3. On success: send to executor; on failure: queue and return executor to pool
///
/// # Unlock Flow
///
/// When executor finishes, unlock accounts and retry blocked transactions in FIFO order.
pub(super) struct ExecutionCoordinator {
    /// Blocked transactions per executor, ordered by transaction ID (FIFO)
    blocked_transactions: Vec<BinaryHeap<TransactionWithId>>,
    /// Pool of idle executors available for work
    ready_executors: Vec<ExecutorId>,
    /// Account locks currently held by each running executor (for unlocking)
    acquired_locks: Vec<Vec<RcLock>>,
    /// Global account lock registry
    locks: LocksCache,
}

impl ExecutionCoordinator {
    pub(super) fn new(count: usize) -> Self {
        Self {
            blocked_transactions: (0..count)
                .map(|_| BinaryHeap::new())
                .collect(),
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

    pub(super) fn try_schedule(
        &mut self,
        executor: ExecutorId,
        txn: TransactionWithId,
    ) -> Result<ProcessableTransaction, ExecutorId> {
        match self.try_acquire_locks(executor, &txn.txn) {
            Ok(()) => Ok(txn.txn),
            Err(blocker) => {
                self.release_executor(executor);
                self.queue_transaction(blocker, txn);
                Err(blocker)
            }
        }
    }

    pub(super) fn try_acquire_locks(
        &mut self,
        executor: ExecutorId,
        txn: &ProcessableTransaction,
    ) -> Result<(), ExecutorId> {
        let message = txn.transaction.message();
        let accounts = message.account_keys();

        for (i, &acc) in accounts.iter().enumerate() {
            let lock = self.locks.entry(acc).or_default().clone();
            let result = if message.is_writable(i) {
                lock.borrow_mut().write(executor)
            } else {
                lock.borrow_mut().read(executor)
            };

            if let Err(blocker) = result {
                // All-or-nothing: release any locks we acquired and fail
                self.unlock_accounts(executor);
                return Err(blocker);
            }
            self.acquired_locks[executor as usize].push(lock);
        }
        Ok(())
    }

    pub(super) fn unlock_accounts(&mut self, executor: ExecutorId) {
        for lock in self.acquired_locks[executor as usize].drain(..) {
            lock.borrow_mut().unlock(executor);
        }
    }

    fn queue_transaction(
        &mut self,
        blocker: ExecutorId,
        txn: TransactionWithId,
    ) {
        let heap = &mut self.blocked_transactions[blocker as usize];
        heap.push(txn);
        MAX_LOCK_CONTENTION_QUEUE_SIZE
            .set(MAX_LOCK_CONTENTION_QUEUE_SIZE.get().max(heap.len() as i64));
    }

    pub(super) fn next_blocked_transaction(
        &mut self,
        executor: ExecutorId,
    ) -> Option<TransactionWithId> {
        self.blocked_transactions[executor as usize].pop()
    }
}

/// Transaction wrapped with a monotonic ID for FIFO queue ordering.
///
/// The ID ensures blocked transactions are processed in arrival order:
/// lower IDs (earlier transactions) are dequeued before higher IDs.
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

// BinaryHeap is max-heap by default, so we reverse comparison for min-heap behavior
impl Ord for TransactionWithId {
    fn cmp(&self, other: &Self) -> Ordering {
        other.id.cmp(&self.id) // smaller ID = higher priority
    }
}
impl PartialOrd for TransactionWithId {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl PartialEq for TransactionWithId {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}
impl Eq for TransactionWithId {}
