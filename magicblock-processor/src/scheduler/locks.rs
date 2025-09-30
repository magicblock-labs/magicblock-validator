//! Fast, in-memory account locking primitives for the multi-threaded scheduler.
//!
//! This version uses a single `u64` bitmask to represent the entire lock state,
//! including read locks, write locks, and contention, for maximum efficiency.

use std::{cell::RefCell, rc::Rc};

use rustc_hash::FxHashMap;
use solana_pubkey::Pubkey;

// A bitmask representing the lock state.
// - MSB: Write lock flag.
// - Remaining bits: Read locks for each executor.
type ReadWriteLock = u64;

/// Unique identifier for a transaction executor worker.
pub(crate) type ExecutorId = u32;

/// Unique identifier for a transaction to be scheduled.
pub(super) type TransactionId = u32;

/// A shared, mutable reference to an `AccountLock`.
pub(super) type RcLock = Rc<RefCell<AccountLock>>;

/// In-memory cache of account locks.
pub(super) type LocksCache = FxHashMap<Pubkey, RcLock>;
/// A map from a blocked transaction to the executor that holds the conflicting lock.
pub(super) type TransactionContention = FxHashMap<TransactionId, ExecutorId>;

/// The maximum number of concurrent executors supported by the bitmask.
/// One bit is reserved for the write flag.
pub(super) const MAX_SVM_EXECUTORS: u32 = ReadWriteLock::BITS - 1;

/// The bit used to indicate a write lock is held. This is the most significant bit.
const WRITE_BIT_MASK: u64 = 1 << (ReadWriteLock::BITS - 1);

/// A read/write lock on a single Solana account, represented by a `u64` bitmask.
#[derive(Default, Debug)]
pub(super) struct AccountLock {
    rw: ReadWriteLock,
    contender: TransactionId,
}

impl AccountLock {
    /// Attempts to acquire a write lock. Fails if any other lock is held.
    pub(super) fn write(
        &mut self,
        executor: ExecutorId,
        txn: TransactionId,
    ) -> Result<(), u32> {
        self.contended(txn)?;
        if self.rw != 0 {
            self.contender = txn;
            // If the lock is held, `trailing_zeros()` will return the index of the
            // least significant bit that is set. This corresponds to the ID of the
            // executor that holds the lock.
            return Err(self.rw.trailing_zeros());
        }
        // Set the write lock bit and the bit for the acquiring executor.
        self.rw = WRITE_BIT_MASK | (1 << executor);
        self.contender = 0;
        Ok(())
    }

    /// Attempts to acquire a read lock. Fails if a write lock is held.
    pub(super) fn read(
        &mut self,
        executor: ExecutorId,
        txn: TransactionId,
    ) -> Result<(), u32> {
        self.contended(txn)?;
        // Check if the write lock bit is set.
        if self.rw & WRITE_BIT_MASK != 0 {
            self.contender = txn;
            // If a write lock is held, the conflicting executor is the one whose
            // bit is set. We can find it using `trailing_zeros()`.
            return Err(self.rw.trailing_zeros());
        }
        // Set the bit corresponding to the executor to acquire a read lock.
        self.rw |= 1 << executor;
        self.contender = 0;
        Ok(())
    }

    /// Releases a lock held by an executor.
    pub(super) fn unlock(&mut self, executor: ExecutorId) {
        // To release the lock, we clear both the write bit and the executor's
        // read bit. This is done using a bitwise AND with the inverted mask.
        self.rw &= !(WRITE_BIT_MASK | (1 << executor));
    }

    /// Checks if the lock is marked as contended by another transaction.
    fn contended(&self, txn: TransactionId) -> Result<(), TransactionId> {
        if self.contender != 0 && self.contender != txn {
            return Err(self.contender);
        }
        Ok(())
    }
}

/// Generates a new, unique transaction ID.
pub(super) fn next_transaction_id() -> TransactionId {
    static mut COUNTER: u32 = MAX_SVM_EXECUTORS;
    // SAFETY: This is safe because the scheduler, which calls this function,
    // operates in a single, dedicated thread. Therefore, there are no concurrent
    // access concerns for this static mutable variable.
    unsafe {
        COUNTER = COUNTER.wrapping_add(1).max(MAX_SVM_EXECUTORS);
        COUNTER
    }
}
