//! Transaction scheduling across multiple executors.
//!
//! Simple locking: try all locks, requeue on failure. No fairness blocking.
//! Transactions ordered by ID (FIFO) within each executor's blocked queue.

use std::{cmp::Ordering, collections::BinaryHeap};

use magicblock_core::link::transactions::ProcessableTransaction;
use magicblock_metrics::metrics::MAX_LOCK_CONTENTION_QUEUE_SIZE;
use solana_pubkey::Pubkey;

use super::locks::{
    next_transaction_id, ExecutorId, LocksCache, TransactionId,
};

/// Transaction tagged with ID for queue ordering.
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

// Min-heap: smaller ID = higher priority
impl Ord for TransactionWithId {
    fn cmp(&self, other: &Self) -> Ordering {
        other.id.cmp(&self.id)
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

pub(super) struct ExecutionCoordinator {
    blocked_transactions: Vec<BinaryHeap<TransactionWithId>>,
    ready_executors: Vec<ExecutorId>,
    /// Account keys locked by each executor (for unlocking)
    held_accounts: Vec<Vec<Pubkey>>,
    locks: LocksCache,
}

impl ExecutionCoordinator {
    pub(super) fn new(count: usize) -> Self {
        Self {
            blocked_transactions: (0..count)
                .map(|_| BinaryHeap::new())
                .collect(),
            held_accounts: (0..count).map(|_| Vec::new()).collect(),
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

    pub(crate) fn try_acquire_locks(
        &mut self,
        executor: ExecutorId,
        txn: &ProcessableTransaction,
    ) -> Result<(), ExecutorId> {
        let message = txn.transaction.message();
        let accounts = message.account_keys();

        for (i, &acc) in accounts.iter().enumerate() {
            let lock = self.locks.entry(acc).or_default();
            let result = if message.is_writable(i) {
                lock.write(executor)
            } else {
                lock.read(executor)
            };

            if let Err(blocker) = result {
                self.unlock_accounts(executor);
                return Err(blocker);
            }
            self.held_accounts[executor as usize].push(acc);
        }
        Ok(())
    }

    pub(crate) fn unlock_accounts(&mut self, executor: ExecutorId) {
        for acc in self.held_accounts[executor as usize].drain(..) {
            if let Some(lock) = self.locks.get_mut(&acc) {
                lock.unlock(executor);
            }
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
