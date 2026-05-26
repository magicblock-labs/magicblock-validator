use lmdb::{Cursor, Error as LmdbError, Iter, RoCursor, RoTransaction};
use solana_pubkey::Pubkey;

use super::{table::Table, MDB_SET_OP};
use crate::{index::Offset, AccountsDbResult};

/// Iterator over pubkeys and offsets.
///
/// This struct bundles the transaction, cursor, and iterator to ensure
/// correct lifetime management for iterating over the index tables.
pub struct OffsetPubkeyIter<'env> {
    /// The underlying LMDB iterator.
    /// Note: Must be dropped before `_cursor` and `_txn`.
    iter: Iter<'env>,
    /// The cursor used by the iterator.
    /// Kept alive to maintain the validity of the iterator.
    _cursor: RoCursor<'env>,
    /// The read-only transaction.
    /// Kept alive to maintain the validity of the cursor.
    _txn: RoTransaction<'env>,
    layout: Layout,
}

#[derive(Clone, Copy)]
enum Layout {
    ProgramValue,
    AccountKey,
}

impl<'env> OffsetPubkeyIter<'env> {
    fn new(
        table: &Table,
        txn: RoTransaction<'env>,
        pubkey: Option<&Pubkey>,
        layout: Layout,
    ) -> AccountsDbResult<Self> {
        let cursor = table.cursor_ro(&txn)?;

        // SAFETY:
        // We are constructing a self-referential struct here:
        // 1. `txn` is moved into the struct.
        // 2. `cursor` borrows `txn`.
        // 3. `iter` borrows `cursor`.
        //
        // Normally, moving `txn` would invalidate the `cursor`. However, LMDB handles
        // (internal C pointers) are stable in memory. We transmute the cursor to
        // extend its lifetime to `'env`, treating it as if it borrows from the
        // environment (which outlives this struct) rather than the local `txn`.
        //
        // Correctness relies on the struct drop order: `iter` -> `_cursor` -> `_txn`.
        let mut cursor: RoCursor<'env> = unsafe { std::mem::transmute(cursor) };

        let iter = if let Some(pubkey) = pubkey {
            // Workaround for LMDB bug with DUPSORT databases where NotFound is ignored
            // during `iter_dup_of`. We explicitly check existence first.
            let key_bytes = pubkey.as_ref();
            match cursor.get(Some(key_bytes), None, MDB_SET_OP) {
                Ok(_) => cursor.iter_dup_of(pubkey),
                Err(LmdbError::NotFound) => Iter::Err(LmdbError::NotFound),
                Err(e) => return Err(e.into()),
            }
        } else {
            cursor.iter_start()
        };

        Ok(Self {
            iter,
            _cursor: cursor,
            _txn: txn,
            layout,
        })
    }

    pub(super) fn new_programs(
        table: &Table,
        txn: RoTransaction<'env>,
        pubkey: Option<&Pubkey>,
    ) -> AccountsDbResult<Self> {
        Self::new(table, txn, pubkey, Layout::ProgramValue)
    }

    pub(super) fn new_accounts(
        table: &Table,
        txn: RoTransaction<'env>,
    ) -> AccountsDbResult<Self> {
        Self::new(table, txn, None, Layout::AccountKey)
    }
}

impl Iterator for OffsetPubkeyIter<'_> {
    type Item = (Offset, Pubkey);

    fn next(&mut self) -> Option<Self::Item> {
        // Retrieve the next record from the LMDB iterator.
        // The iterator returns `(&[u8], &[u8])` representing (key, value).
        let (key_bytes, value_bytes) = self.iter.next()?.ok()?;

        match self.layout {
            Layout::ProgramValue => {
                // `programs` values are `[offset | pubkey]`.
                Some(bytes!(#unpack, value_bytes, Offset, Pubkey))
            }
            Layout::AccountKey => {
                // `accounts` keys are the pubkey; values are `[offset | blocks]`.
                let pubkey = Pubkey::try_from(key_bytes).ok()?;
                // SAFETY: In the `Layout::AccountKey` path, `value_bytes` comes
                // from the `accounts` index, whose values are always encoded as
                // `[Offset | Blocks]`. That guarantees `value_bytes` has at least
                // `size_of::<Offset>()` bytes, and `read_unaligned` does not
                // require aligned input. The first bytes therefore represent a
                // valid `Offset` for the `Pubkey::try_from(key_bytes)` entry.
                let offset = unsafe {
                    (value_bytes.as_ptr() as *const Offset).read_unaligned()
                };
                Some((offset, pubkey))
            }
        }
    }
}
