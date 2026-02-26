//! Transaction scheduling coordination across multiple executors.
//!
//! # Architecture
//!
//! - **Lock acquisition**: All-or-nothing attempt on all transaction accounts
//! - **Lock contention**: Failed transactions queued behind blocking executor
//! - **FIFO ordering**: Transactions processed in ID order within each blocked queue
//!
//! # Coordination Modes
//!
//! The scheduler operates in one of two modes:
//! - **Primary**: For validators accepting client transactions. Allows concurrent
//!   execution of independent transactions, with a limit on blocked transactions.
//! - **Replica**: For validators replaying transactions from a primary. Enforces
//!   strict ordering by allowing only one pending blocked transaction at a time.
//!
//! # Key Types
//!
//! - `ExecutorId`: Unique identifier for each executor worker (0..N)
//! - `TransactionId`: Monotonic ID for FIFO queue ordering
//! - `BinaryHeap<TransactionWithId>`: Min-heap ordered by transaction ID

use std::{cmp::Ordering, collections::BinaryHeap};

use magicblock_core::link::transactions::{
    ProcessableTransaction, TransactionProcessingMode,
};
use magicblock_metrics::metrics::MAX_LOCK_CONTENTION_QUEUE_SIZE;
use tracing::{error, warn};

use super::locks::{
    next_transaction_id, ExecutorId, LocksCache, TransactionId,
};
use crate::scheduler::locks::RcLock;

/// Maximum blocked transactions per executor before rejecting new ones (Primary mode).
const BLOCKED_TXN_MULTIPLIER: usize = 2;

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
    /// Current coordination mode (Primary or Replica)
    mode: CoordinationMode,
}

/// Coordination mode determining how transactions are scheduled.
pub(super) enum CoordinationMode {
    /// Primary mode: accepts client transactions, allows concurrent execution.
    Primary(PrimaryMode),
    /// Replica mode: replays transactions, enforces strict ordering.
    Replica(ReplicaMode),
}

/// State for Primary mode scheduling.
pub(super) struct PrimaryMode {
    /// Current number of blocked transactions.
    blocked_txn_count: usize,
    /// Maximum allowed blocked transactions before rejecting new ones.
    max_blocked_txn: usize,
}

/// State for Replica mode scheduling.
///
/// In Replica mode, only one transaction can be pending (blocked) at a time.
/// This ensures strict ordering during replay: when a transaction is blocked,
/// new transactions wait in the channel until it completes.
#[derive(Default)]
pub(super) struct ReplicaMode {
    /// ID of the currently pending blocked transaction, if any.
    pending: Option<TransactionId>,
}

impl CoordinationMode {
    /// Returns true if the scheduler is ready to accept new transactions.
    fn is_ready(&self) -> bool {
        match self {
            // Primary: ready if we haven't hit the blocked transaction limit
            Self::Primary(m) => m.blocked_txn_count < m.max_blocked_txn,
            // Replica: ready only if no transaction is pending (strict ordering)
            Self::Replica(m) => m.pending.is_none(),
        }
    }
}

impl ExecutionCoordinator {
    /// Creates a new coordinator starting in Replica mode.
    ///
    /// Starts in Replica mode to allow ledger replay before switching to Primary.
    pub(super) fn new(count: usize) -> Self {
        Self {
            blocked_transactions: (0..count)
                .map(|_| BinaryHeap::new())
                .collect(),
            acquired_locks: (0..count).map(|_| Vec::new()).collect(),
            ready_executors: (0..count as u32).collect(),
            locks: LocksCache::default(),
            mode: CoordinationMode::Replica(ReplicaMode::default()),
        }
    }

    #[inline]
    pub(super) fn is_ready(&self) -> bool {
        !self.ready_executors.is_empty() && self.mode.is_ready()
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
        match &mut self.mode {
            CoordinationMode::Replica(r) => {
                // In Replica mode, track the pending transaction ID.
                // The debug_assert ensures we don't queue when one is already pending
                // (enforced by is_ready() returning false when pending.is_some()).
                debug_assert!(r.pending.is_none());
                if r.pending.replace(txn.id).is_some() {
                    error!("Invariant violation: replaced pending transaction");
                }
            }
            CoordinationMode::Primary(p) => {
                p.blocked_txn_count += 1;
            }
        }
        let heap = &mut self.blocked_transactions[blocker as usize];
        heap.push(txn);
        MAX_LOCK_CONTENTION_QUEUE_SIZE
            .set(MAX_LOCK_CONTENTION_QUEUE_SIZE.get().max(heap.len() as i64));
    }

    pub(super) fn next_blocked_transaction(
        &mut self,
        executor: ExecutorId,
    ) -> Option<TransactionWithId> {
        let txn = self.blocked_transactions[executor as usize].pop();
        match &mut self.mode {
            CoordinationMode::Replica(r) => {
                // Clear pending if this was the pending transaction.
                if r.pending == txn.as_ref().map(|txn| txn.id) {
                    r.pending.take();
                }
            }
            CoordinationMode::Primary(p) => {
                p.blocked_txn_count =
                    p.blocked_txn_count.saturating_sub(txn.is_some() as usize);
            }
        }
        txn
    }

    /// Switches from Replica to Primary mode.
    ///
    /// Called after ledger replay completes on Primary validators.
    /// No-op if already in Primary mode.
    pub(super) fn switch_to_primary_mode(&mut self) {
        if let CoordinationMode::Primary(_) = self.mode {
            warn!("Tried to switch to primary mode more than once");
            return;
        }
        let mode = PrimaryMode {
            blocked_txn_count: 0,
            max_blocked_txn: self.blocked_transactions.len()
                * BLOCKED_TXN_MULTIPLIER,
        };
        self.mode = CoordinationMode::Primary(mode);
    }

    /// Checks if a transaction mode is compatible with the current coordination mode.
    ///
    /// - Primary mode: rejects Replay transactions (only client Execution allowed)
    /// - Replica mode: rejects Execution transactions (only Replay allowed)
    /// - Simulations are allowed in all modes
    pub(super) fn is_transaction_allowed(
        &self,
        mode: &TransactionProcessingMode,
    ) -> bool {
        use CoordinationMode::*;
        use TransactionProcessingMode::*;
        let mode_mismatch = matches!(
            (&self.mode, mode),
            (Primary(_), Replay(_)) | (Replica(_), Execution(_))
        );
        !mode_mismatch
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
