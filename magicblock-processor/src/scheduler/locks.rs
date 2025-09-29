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

/// A shared, mutable reference to an `AccountLock`.
pub(crate) type RcLock = Rc<RefCell<AccountLock>>;

/// In-memory cache of account locks.
pub(crate) type LocksCache = FxHashMap<Pubkey, RcLock>;

/// The maximum number of concurrent executors supported by the bitmask.
/// Two bits are reserved for the write and contention flags.
pub(crate) const MAX_SVM_EXECUTORS: u32 = ReadWriteLock::BITS - 2;

/// The bit used to indicate a write lock is held.
const WRITE_BIT_MASK: u64 = 1 << (ReadWriteLock::BITS - 1);

/// The bit used to indicate that the lock is contended.
const CONTENTION_BIT_MASK: u64 = 1 << (ReadWriteLock::BITS - 2);

/// A read/write lock on a single Solana account, represented by a `u64` bitmask.
#[derive(Default, Debug)]
#[repr(transparent)]
pub(crate) struct AccountLock(ReadWriteLock);

impl AccountLock {
    /// Attempts to acquire a write lock. Fails if any other lock is held.
    pub(crate) fn write(
        &mut self,
        executor: ExecutorId,
    ) -> Result<(), ExecutorId> {
        if self.0 != 0 {
            self.contend();
            return Err(self.0.trailing_zeros());
        }
        // Acquiring the lock clears any previous contention.
        self.0 = WRITE_BIT_MASK | (1 << executor);
        Ok(())
    }

    /// Attempts to acquire a read lock. Fails if a write lock is held or if the
    /// lock is marked as contended.
    pub(crate) fn read(
        &mut self,
        executor: ExecutorId,
    ) -> Result<(), ExecutorId> {
        if self.0 & WRITE_BIT_MASK != 0 || self.is_contended() {
            return Err(self.0.trailing_zeros());
        }
        self.0 |= 1 << executor;
        Ok(())
    }

    /// Releases a lock held by an executor.
    pub(crate) fn unlock(&mut self, executor: ExecutorId) {
        // Clear the executor's read bit and the global write bit
        self.0 &= !(WRITE_BIT_MASK | CONTENTION_BIT_MASK | (1 << executor));
    }

    /// Checks if the lock is marked as contended.
    fn is_contended(&self) -> bool {
        self.0 & CONTENTION_BIT_MASK != 0
    }

    /// Marks the lock as contended when an acquisition fails.
    fn contend(&mut self) {
        self.0 |= CONTENTION_BIT_MASK;
    }
}
