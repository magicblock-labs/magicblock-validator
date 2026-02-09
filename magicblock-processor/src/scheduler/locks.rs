//! Fast, in-memory account locking for the scheduler.
//!
//! # Bitmask Layout
//!
//! Each `AccountLock` uses a single `u64`:
//! - **MSB (bit 63)**: Write lock flag (exclusive access)
//! - **Bits 0-62**: Read lock flags per executor (concurrent access)
//!
//! # Concurrency Rules
//!
//! - **Write lock**: Requires zero state (no readers or writers)
//! - **Read lock**: Requires MSB clear (no active writer)
//! - **Unlock**: Clears both write bit and executor's read bit
//!
//! # Limits
//!
//! Maximum 63 executors (u64::BITS - 1, reserving MSB for write flag)

use std::{
    cell::RefCell,
    rc::Rc,
    sync::atomic::{AtomicU64, Ordering},
};

use rustc_hash::FxHashMap;
use solana_pubkey::Pubkey;

pub(crate) type ExecutorId = u32;
pub(super) type TransactionId = u64;
pub(super) type RcLock = Rc<RefCell<AccountLock>>;
pub(super) type LocksCache = FxHashMap<Pubkey, RcLock>;

pub(super) const MAX_SVM_EXECUTORS: u32 = u64::BITS - 1;
const WRITE_BIT: u64 = 1 << MAX_SVM_EXECUTORS;

pub(super) fn next_transaction_id() -> TransactionId {
    // Relaxed ordering is sufficient: IDs only need to be unique per transaction
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    COUNTER.fetch_add(1, Ordering::Relaxed)
}

/// Read/write lock on a single account using bitmask state.
///
/// See [module-level documentation](self) for bitmask layout details.
#[derive(Default)]
pub(super) struct AccountLock {
    /// Bitmask lock state (see module docs for layout)
    state: u64,
}

impl AccountLock {
    #[inline]
    pub(super) fn write(
        &mut self,
        executor: ExecutorId,
    ) -> Result<(), ExecutorId> {
        if self.state != 0 {
            // trailing_zeros() returns index of first set bit (the blocking executor)
            return Err(self.state.trailing_zeros());
        }
        // Set write bit AND executor bit to track ownership
        self.state = WRITE_BIT | (1 << executor);
        Ok(())
    }

    #[inline]
    pub(super) fn read(
        &mut self,
        executor: ExecutorId,
    ) -> Result<(), ExecutorId> {
        if self.state & WRITE_BIT != 0 {
            // trailing_zeros() returns the writer's executor ID
            return Err(self.state.trailing_zeros());
        }
        self.state |= 1 << executor;
        Ok(())
    }

    #[inline]
    pub(super) fn unlock(&mut self, executor: ExecutorId) {
        // Clear both write bit (if set) and executor's read bit
        self.state &= !(WRITE_BIT | (1 << executor));
    }
}
