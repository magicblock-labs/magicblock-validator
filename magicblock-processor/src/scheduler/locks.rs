//! Fast, in-memory account locking primitives for the multi-threaded scheduler.
//!
//! This module implements a custom bitmask-based locking mechanism to manage
//! concurrent access to Solana accounts. It optimizes for memory footprint and
//! speed by representing the lock state of an account as a single `u64`.
//!
//! # State Layout (`u64`)
//! - **MSB (Most Significant Bit):** Write lock flag. If set, a write lock is held.
//! - **Remaining Bits (0..62):** Read lock flags. Each bit corresponds to a specific
//!   executor ID. If bit `i` is set, executor `i` holds a read lock.
//!
//! This design supports up to 63 concurrent executors (0-62), which exceeds the
//! typical number of cores in a validator (where the limiting factor is usually SVM execution).

use std::{cell::RefCell, rc::Rc};

use rustc_hash::FxHashMap;
use solana_pubkey::Pubkey;

/// Unique identifier for a transaction executor worker.
pub(crate) type ExecutorId = u32;

/// A shared, mutable reference to an `AccountLock`.
pub(super) type RcLock = Rc<RefCell<AccountLock>>;

/// Global cache of account locks, indexed by account address.
pub(super) type LocksCache = FxHashMap<Pubkey, RcLock>;

/// The maximum number of concurrent executors supported by the bitmask.
pub(super) const MAX_SVM_EXECUTORS: u32 = u64::BITS - 1;

/// The bitmask for the write lock (the most significant bit).
const WRITE_BIT_MASK: u64 = 1 << MAX_SVM_EXECUTORS;

/// A read/write lock on a single Solana account, represented by a `u64` bitmask.
#[derive(Default, Debug)]
pub(super) struct AccountLock {
    state: u64,
}

impl AccountLock {
    /// Attempts to acquire a write lock for the given executor.
    /// Returns the blocking executor's ID on failure.
    #[inline]
    pub(super) fn write(
        &mut self,
        executor: ExecutorId,
    ) -> Result<(), ExecutorId> {
        if self.state != 0 {
            return Err(self.state.trailing_zeros());
        }
        self.state = WRITE_BIT_MASK | (1 << executor);
        Ok(())
    }

    /// Attempts to acquire a read lock for the given executor.
    /// Returns the blocking executor's ID if a write lock is active.
    #[inline]
    pub(super) fn read(
        &mut self,
        executor: ExecutorId,
    ) -> Result<(), ExecutorId> {
        if self.state & WRITE_BIT_MASK != 0 {
            return Err(self.state.trailing_zeros());
        }
        self.state |= 1 << executor;
        Ok(())
    }

    /// Releases any lock held by the specified executor.
    #[inline]
    pub(super) fn unlock(&mut self, executor: ExecutorId) {
        self.state &= !(WRITE_BIT_MASK | (1 << executor));
    }
}
