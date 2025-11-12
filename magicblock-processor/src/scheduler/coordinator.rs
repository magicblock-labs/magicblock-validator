//! Manages the state of transaction processing across multiple executors.
//!
//! This module contains the `ExecutionCoordinator`, which tracks ready executors,
//! queues of blocked transactions, and the locks held by each worker. It acts
//! as the central state machine for the scheduling process.

use std::collections::VecDeque;

use magicblock_core::link::transactions::ProcessableTransaction;
use solana_pubkey::Pubkey;

use super::locks::{
    next_transaction_id, AccountContention, BlockerId, ExecutorId, LocksCache,
    RcLock, TransactionContention, TransactionId,
};

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
    /// A map tracking which transactions are contending for which account.
    account_contention: AccountContention,
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
            account_contention: AccountContention::default(),
            locks: LocksCache::default(),
        }
    }

    /// Queues a transaction that is blocked by a contended lock.
    ///
    /// The `blocker_id` can be either an `ExecutorId` or a `TransactionId`.
    /// If it's a `TransactionId`, this function resolves it to the underlying
    /// `ExecutorId` that holds the conflicting lock, and returns that executor
    pub(super) fn queue_transaction(
        &mut self,
        blocker: BlockerId,
        transaction: TransactionWithId,
    ) -> ExecutorId {
        let executor = match blocker {
            BlockerId::Executor(executor) => executor,
            BlockerId::Transaction(id) => {
                // A `TransactionId` is only returned as a blocker if that
                // transaction is already tracked in the contention map.
                self.transaction_contention
                    .get(&id)
                    .copied()
                    // SAFETY:
                    // This invariant is enforced via careful transaction scheduling
                    // flow, if the transaction is not found in the map, this indicates a
                    // hard logic error, which might lead to deadlocks, thus we terminate
                    // the scheduler here. Test coverage should catch this inconsistency.
                    .expect("unknown transaction for blocker resolution")
            }
        };
        let queue = &mut self.blocked_transactions[executor as usize];
        self.transaction_contention.insert(transaction.id, executor);
        let index = queue.binary_search_by(|tx| tx.id.cmp(&transaction.id));
        if let Err(index) = index {
            queue.insert(index, transaction);
        }
        executor
    }

    /// Checks if there are any executors ready to process a transaction.
    #[inline]
    pub(super) fn is_ready(&self) -> bool {
        !self.ready_executors.is_empty()
    }

    /// Retrieves the ID of a ready executor, if one is available.
    #[inline]
    pub(super) fn get_ready_executor(&mut self) -> Option<ExecutorId> {
        self.ready_executors.pop()
    }

    /// Returns an executor to the pool of ready executors.
    #[inline]
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
    pub(super) fn next_blocked_transaction(
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
    ) -> Result<(), BlockerId> {
        let message = transaction.txn.transaction.message();
        let accounts_to_lock = message.account_keys().iter().enumerate();
        let acquired_locks = &mut self.acquired_locks[executor as usize];

        for (i, &acc) in accounts_to_lock.clone() {
            // Get or create the lock for the account.
            let lock = self.locks.entry(acc).or_default().clone();
            // See whether there's a contention for the given account
            // if there's one, then we need to follow the breadcrumbs
            // (txns->executor) to find out where our transaction
            // needs to be queued.
            let mut result =
                if let Some(contenders) = self.account_contention.get(&acc) {
                    match contenders.binary_search(&transaction.id) {
                        // If we are the first contender, then we can proceed
                        // and try to acquire the needed account locks
                        // 1. Ok(0) => the transaction has already contended this account,
                        //             and now it is its turn to be scheduled
                        // 2. Err(0) => the transaction didn't contend the account, but it
                        //              has smaller ID (was received earlier), so we don't
                        //              block it and let it proceed with lock acquisition
                        Ok(0) | Err(0) => Ok(()),
                        // If we are not, then we need to get queued after
                        // the transaction contending right in front of us
                        Ok(index) | Err(index) => {
                            Err(BlockerId::Transaction(contenders[index - 1]))
                        }
                    }
                } else {
                    Ok(())
                };

            if result.is_ok() {
                // Attempt to acquire a write or read lock.
                result = if message.is_writable(i) {
                    lock.borrow_mut().write(executor)
                } else {
                    lock.borrow_mut().read(executor)
                }
                .map_err(BlockerId::Executor);
            }

            // We couldn't lock all of the accounts, so we are bailing, but
            // first we need to set contention, and unlock successful locks
            if let Err(e) = result {
                for lock in acquired_locks.drain(..) {
                    let mut lock = lock.borrow_mut();
                    lock.unlock(executor);
                }
                for (i, &acc) in accounts_to_lock {
                    // We only set contention for write locks,
                    // in order to prevent writer starvation
                    if message.is_writable(i) {
                        self.contend_account(acc, transaction.id);
                    }
                }
                return Err(e);
            }

            acquired_locks.push(lock);
        }

        // On success, the transaction is no longer blocking anything.
        self.transaction_contention.remove(&transaction.id);
        for (_, acc) in accounts_to_lock {
            self.clear_account_contention(acc, transaction.id);
        }
        Ok(())
    }

    /// Tries to acquire all the necessary account locks, required for
    /// transaction execution. If that fails, the executor ID, currently
    /// holding one of the account locks is returned as Err.
    pub(super) fn try_schedule(
        &mut self,
        executor: ExecutorId,
        txn: TransactionWithId,
    ) -> Result<ProcessableTransaction, ExecutorId> {
        let blocker = self.try_acquire_locks(executor, &txn);
        if let Err(blocker) = blocker {
            self.release_executor(executor);
            let blocker = self.queue_transaction(blocker, txn);
            return Err(blocker);
        }
        Ok(txn.txn)
    }

    /// Sets the transaction contention for this account. Contenders are ordered
    /// based on their ID, which honours the "first in, first served" policy
    #[inline]
    fn contend_account(&mut self, acc: Pubkey, txn: TransactionId) {
        let contenders = self.account_contention.entry(acc).or_default();
        if let Err(index) = contenders.binary_search(&txn) {
            contenders.insert(index, txn);
        }
    }

    /// Removes the given transaction from contenders list for the specified account
    #[inline]
    fn clear_account_contention(&mut self, acc: &Pubkey, txn: TransactionId) {
        let Some(contenders) = self.account_contention.get_mut(acc) else {
            return;
        };
        if let Ok(index) = contenders.binary_search(&txn) {
            contenders.remove(index);
        }
        // Prevent unbounded growth of tracking map
        if contenders.is_empty() {
            self.account_contention.remove(acc);
        }
    }
}
