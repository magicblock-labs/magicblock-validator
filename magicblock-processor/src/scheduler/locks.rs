//! Fast, in-memory account locking primitives for the multi-threaded scheduler.
//!
//! This version uses a single `u64` bitmask to represent the entire lock state,
//! including read locks, write locks, and contention, for maximum efficiency.

use std::{cell::RefCell, collections::VecDeque, rc::Rc};

use rustc_hash::FxHashMap;
use solana_pubkey::Pubkey;

// A bitmask representing the lock state.
// - MSB: Write lock flag.
// - Remaining bits: Read locks for each executor.
type ReadWriteLock = u64;

/// Unique identifier for a transaction executor worker.
pub(crate) type ExecutorId = u32;

/// Unique identifier for a transaction to be scheduled.
///
/// NOTE: the type is specifically set to u64, so that it
/// will be statistically impossible to overlow it within
/// next few millenia, given the indended use case of the
/// type as a tagging counter for incoming transactions.
pub(super) type TransactionId = u64;

/// A shared, mutable reference to an `AccountLock`.
pub(super) type RcLock = Rc<RefCell<AccountLock>>;

/// In-memory cache of account locks.
pub(super) type LocksCache = FxHashMap<Pubkey, RcLock>;
/// A map from a blocked transaction to the executor that holds the conflicting lock.
pub(super) type TransactionContention = FxHashMap<TransactionId, ExecutorId>;
/// A map from account's pubkey to all transaction, that are contending to acquire its lock.
pub(super) type AccountContention = FxHashMap<Pubkey, VecDeque<TransactionId>>;

/// The maximum number of concurrent executors supported by the bitmask.
/// One bit is reserved for the write flag.
pub(super) const MAX_SVM_EXECUTORS: u32 = ReadWriteLock::BITS - 1u32;

/// The bit used to indicate a write lock is held. This is the most significant bit.
const WRITE_BIT_MASK: u64 = 1u64 << (ReadWriteLock::BITS - 1u32);

/// A read/write lock on a single Solana account, represented by a `u64` bitmask.
#[derive(Default, Debug)]
pub(super) struct AccountLock {
    rw: ReadWriteLock,
}

/// An ID of the entity that is blocking transaction execution:
/// 1. Executor: one of the executors is holding the necessary account lock(s)
/// 2. Transaction: some transaction has already contended necessary account
///    lock and needs to be executed first, so we need to be queued after it.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum BlockerId {
    Executor(ExecutorId),
    Transaction(TransactionId),
}

impl AccountLock {
    /// Attempts to acquire a write lock. Fails if any other lock is held.
    /// The return Err value corresponds to the exeucotor ID, holding the lock
    #[inline]
    pub(super) fn write(
        &mut self,
        executor: ExecutorId,
    ) -> Result<(), ExecutorId> {
        if self.rw != 0 {
            // If the lock is held, `trailing_zeros()` will return
            // the index of the least significant bit that is set.
            return Err(self.rw.trailing_zeros());
        }
        // Set the write lock bit and the bit for the acquiring executor.
        self.rw = WRITE_BIT_MASK | (1u64 << executor);
        Ok(())
    }

    /// Attempts to acquire a read lock. Fails if a write lock is held.
    /// The return Err value corresponds to the exeucotor ID, holding the lock
    #[inline]
    pub(super) fn read(
        &mut self,
        executor: ExecutorId,
    ) -> Result<(), ExecutorId> {
        // Check if the write lock bit is set.
        if self.rw & WRITE_BIT_MASK != 0 {
            // If a write lock is held, the conflicting executor is the one whose
            // bit is set. We can find it using `trailing_zeros()`.
            return Err(self.rw.trailing_zeros());
        }
        // Set the bit corresponding to the executor to acquire a read lock.
        self.rw |= 1u64 << executor;
        Ok(())
    }

    /// Releases a lock held by an executor.
    #[inline]
    pub(super) fn unlock(&mut self, executor: ExecutorId) {
        // To release the lock, we clear both the write bit and the executor's
        // read bit. This is done using a bitwise AND with the inverted mask.
        self.rw &= !(WRITE_BIT_MASK | (1u64 << executor));
    }
}

/// Generates a new, unique transaction ID.
pub(super) fn next_transaction_id() -> TransactionId {
    static mut COUNTER: TransactionId = 0;
    // SAFETY: This is safe because the scheduler, which calls this function,
    // operates in a single, dedicated thread. Therefore, there are no concurrent
    // access concerns for this static mutable variable. The u64::MAX is large
    // enough range to statistically guarantee that no two transactions created
    // during the lifetime of the validator have the same ID.
    unsafe {
        COUNTER = COUNTER.wrapping_add(1);
        COUNTER
    }
}
