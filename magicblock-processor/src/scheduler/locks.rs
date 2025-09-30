//! Fast, in-memory account locking primitives for the multi-threaded scheduler.
//!
//! This version uses a single `u64` bitmask to represent the entire lock state,
//! including read locks, write locks, and contention, for maximum efficiency.

use std::{cell::RefCell, rc::Rc};

use rustc_hash::FxHashMap;
use solana_pubkey::Pubkey;

// A bitmask representing the lock state.
// - MSB: Write lock flag.
// - MSB-1: Contention flag.
// - Remaining bits: Read locks for each executor.
type ReadWriteLock = u64;

/// Unique identifier for a transaction executor worker.
pub(crate) type ExecutorId = u32;

pub(super) type TransactionId = u32;

/// A shared, mutable reference to an `AccountLock`.
pub(super) type RcLock = Rc<RefCell<AccountLock>>;

/// In-memory cache of account locks.
pub(super) type LocksCache = FxHashMap<Pubkey, RcLock>;
pub(super) type TransactionQueues = FxHashMap<TransactionId, ExecutorId>;

/// The maximum number of concurrent executors supported by the bitmask.
/// Two bits are reserved for the write and contention flags.
pub(super) const MAX_SVM_EXECUTORS: u32 = ReadWriteLock::BITS - 1;

/// The bit used to indicate a write lock is held.
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
    ) -> Result<(), ExecutorId> {
        self.contended(txn)?;
        if self.rw != 0 {
            self.contender = txn;
            return Err(self.rw.trailing_zeros());
        }
        self.rw = WRITE_BIT_MASK | (1 << executor);
        self.contender = 0;
        Ok(())
    }

    /// Attempts to acquire a read lock. Fails if a write lock is held or if the
    /// lock is marked as contended.
    pub(super) fn read(
        &mut self,
        executor: ExecutorId,
        txn: TransactionId,
    ) -> Result<(), ExecutorId> {
        self.contended(txn)?;
        if self.rw & WRITE_BIT_MASK != 0 {
            self.contender = txn;
            return Err(self.rw.trailing_zeros());
        }
        self.rw |= 1 << executor;
        self.contender = 0;
        Ok(())
    }

    /// Releases a lock held by an executor.
    pub(super) fn unlock(&mut self, executor: ExecutorId) {
        // Clear the executor's read bit and the global write bit
        self.rw &= !(WRITE_BIT_MASK | (1 << executor));
    }

    /// Checks if the lock is marked as contended.
    fn contended(&self, txn: TransactionId) -> Result<(), TransactionId> {
        if self.contender != 0 && self.contender != txn {
            return Err(self.contender);
        }
        Ok(())
    }
}

pub(super) fn next_transaction_id() -> TransactionId {
    static mut COUNTER: u32 = MAX_SVM_EXECUTORS;
    // # SAFETY: This is safe because the scheduler operates in a single thread.
    unsafe {
        COUNTER = COUNTER.wrapping_add(1).max(MAX_SVM_EXECUTORS);
        COUNTER
    }
}
