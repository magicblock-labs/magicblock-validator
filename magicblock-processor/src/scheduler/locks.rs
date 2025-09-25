use std::{cell::RefCell, rc::Rc};

use rustc_hash::FxHashMap;
use solana_pubkey::Pubkey;

pub(crate) type WorkerId = u32;

pub type LocksCache = FxHashMap<Pubkey, Rc<RefCell<AccountLock>>>;

type ReadWriteLock = u64;
#[derive(Clone, Copy, Default)]
#[repr(transparent)]
struct TransactionId(u32);

pub(crate) const MAX_SVM_WORKERS: u32 = ReadWriteLock::BITS - 1;
const WRITE_BIT_MASK: u64 = 1 << (ReadWriteLock::BITS - 1);

impl TransactionId {
    pub(crate) fn next() -> Self {
        static mut COUNTER: u32 = 0;
        let id = unsafe {
            COUNTER = COUNTER.wrapping_add(1).max(1);
            COUNTER
        };
        Self(id)
    }

    fn clear(&mut self) {
        self.0 = 0;
    }
}

#[derive(Default)]
pub(crate) struct AccountLock {
    rw: ReadWriteLock,
    contender: TransactionId,
}

impl AccountLock {
    pub(crate) fn write(
        &mut self,
        worker: WorkerId,
        txn: TransactionId,
    ) -> Result<(), WorkerId> {
        if self.rw != 0 {
            self.contend(txn);
            return Err(self.rw.trailing_zeros());
        }
        self.contender.clear();
        self.rw |= WRITE_BIT_MASK | (1 << worker);
        Ok(())
    }

    pub(crate) fn read(
        &mut self,
        worker: WorkerId,
        txn: TransactionId,
    ) -> Result<(), WorkerId> {
        if self.rw & WRITE_BIT_MASK != 0 {
            self.contend(txn);
            return Err(self.rw.trailing_zeros());
        }
        self.contender.clear();
        self.rw |= 1 << worker;
        Ok(())
    }

    pub(crate) fn unlock(&mut self, worker: WorkerId) {
        self.rw &= !(WRITE_BIT_MASK | (1 << worker));
    }

    fn contended(&self, txn: TransactionId) -> bool {
        return self.contender.0 != 0 && self.contender.0 == txn.0;
    }

    fn contend(&mut self, txn: TransactionId) {
        if self.contended(txn) {
            return;
        }
        self.contender = txn;
    }
}
