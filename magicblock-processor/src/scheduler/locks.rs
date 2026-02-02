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

use std::{
    cell::RefCell,
    collections::VecDeque,
    rc::Rc,
    sync::atomic::{AtomicU64, Ordering},
};

use rustc_hash::FxHashMap;
use solana_pubkey::Pubkey;

/// Unique identifier for a transaction executor worker.
pub(crate) type ExecutorId = u32;

/// Unique identifier for a transaction (monotonically increasing).
pub(super) type TransactionId = u64;

/// A shared, mutable reference to an `AccountLock`.
pub(super) type RcLock = Rc<RefCell<AccountLock>>;

/// Global cache of account locks, indexed by account address.
pub(super) type LocksCache = FxHashMap<Pubkey, RcLock>;

// --- Contention Tracking Maps ---
/// Maps a blocked transaction ID to the Executor ID blocking it.
pub(super) type TransactionContention = FxHashMap<TransactionId, ExecutorId>;
/// Maps an account to a queue of transactions waiting to lock it.
pub(super) type AccountContention = FxHashMap<Pubkey, VecDeque<TransactionId>>;

/// The maximum number of concurrent executors supported by the bitmask.
/// We reserve the most significant bit for the write lock flag.
pub(super) const MAX_SVM_EXECUTORS: u32 = u64::BITS - 1;

/// The bitmask for the write lock (the most significant bit).
const WRITE_BIT_MASK: u64 = 1 << MAX_SVM_EXECUTORS;

/// A read/write lock on a single Solana account, represented by a `u64` bitmask.
#[derive(Default, Debug)]
pub(super) struct AccountLock {
    /// Bitmask representing the lock state.
    state: u64,
}

/// Identifies the entity preventing a lock acquisition.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum BlockerId {
    /// An executor is currently holding a conflicting lock.
    Executor(ExecutorId),
    /// A transaction is ahead in the queue for this lock (fairness enforcement).
    Transaction(TransactionId),
}

impl AccountLock {
    /// Attempts to acquire a write lock for the given executor.
    ///
    /// # Errors
    /// Returns the `ExecutorId` of the current lock holder if the lock cannot be acquired.
    /// A write lock requires the state to be completely 0 (no readers, no writers).
    #[inline]
    pub(super) fn write(
        &mut self,
        executor: ExecutorId,
    ) -> Result<(), ExecutorId> {
        if self.state != 0 {
            // trailing_zeros() efficiently finds the index of the first set bit,
            // which corresponds to the ID of a blocking executor (reader or writer).
            return Err(self.state.trailing_zeros());
        }
        // Set the write bit AND the executor's bit to indicate ownership.
        self.state = WRITE_BIT_MASK | (1 << executor);
        Ok(())
    }

    /// Attempts to acquire a read lock for the given executor.
    ///
    /// # Errors
    /// Returns the `ExecutorId` of the current write lock holder if a write lock is active.
    /// Multiple read locks can coexist, provided the write bit is not set.
    #[inline]
    pub(super) fn read(
        &mut self,
        executor: ExecutorId,
    ) -> Result<(), ExecutorId> {
        // Check if the write lock bit is set.
        if self.state & WRITE_BIT_MASK != 0 {
            return Err(self.state.trailing_zeros());
        }
        // Set the executor's specific bit to register the read lock.
        self.state |= 1 << executor;
        Ok(())
    }

    /// Releases the lock (read or write) held by the specified executor.
    #[inline]
    pub(super) fn unlock(&mut self, executor: ExecutorId) {
        // Clear both the write bit (if set) and the executor's specific bit.
        // Using `&!` (AND NOT) is efficient and handles both read and write unlock logic identically
        // because we essentially "subtract" this executor's presence from the mask.
        self.state &= !(WRITE_BIT_MASK | (1 << executor));
    }
}

/// Generates a new, unique transaction ID using an atomic counter.
pub(super) fn next_transaction_id() -> TransactionId {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    COUNTER.fetch_add(1, Ordering::Relaxed)
}
