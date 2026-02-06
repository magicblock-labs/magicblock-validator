//! Fast, in-memory account locking primitives for the scheduler.
//!
//! Bitmask-based locking: a single `u64` represents lock state per account.
//! - MSB: Write lock flag
//! - Bits 0..62: Read lock flags per executor

use std::sync::atomic::{AtomicU64, Ordering};

use rustc_hash::FxHashMap;
use solana_pubkey::Pubkey;

pub(crate) type ExecutorId = u32;
pub(super) type TransactionId = u64;
pub(super) type LocksCache = FxHashMap<Pubkey, AccountLock>;

pub(super) const MAX_SVM_EXECUTORS: u32 = u64::BITS - 1;
const WRITE_BIT: u64 = 1 << MAX_SVM_EXECUTORS;

pub(super) fn next_transaction_id() -> TransactionId {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    COUNTER.fetch_add(1, Ordering::Relaxed)
}

#[derive(Default)]
pub(super) struct AccountLock {
    state: u64,
}

impl AccountLock {
    #[inline]
    pub(super) fn write(
        &mut self,
        executor: ExecutorId,
    ) -> Result<(), ExecutorId> {
        if self.state != 0 {
            return Err(self.state.trailing_zeros());
        }
        self.state = WRITE_BIT | (1 << executor);
        Ok(())
    }

    #[inline]
    pub(super) fn read(
        &mut self,
        executor: ExecutorId,
    ) -> Result<(), ExecutorId> {
        if self.state & WRITE_BIT != 0 {
            return Err(self.state.trailing_zeros());
        }
        self.state |= 1 << executor;
        Ok(())
    }

    #[inline]
    pub(super) fn unlock(&mut self, executor: ExecutorId) {
        self.state &= !(WRITE_BIT | (1 << executor));
    }
}
