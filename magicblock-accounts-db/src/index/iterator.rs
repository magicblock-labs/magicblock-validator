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
}

impl<'env> OffsetPubkeyIter<'env> {
    pub(super) fn new(
        table: &Table,
        txn: RoTransaction<'env>,
        pubkey: Option<&Pubkey>,
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
        })
    }
}

impl Iterator for OffsetPubkeyIter<'_> {
    type Item = (Offset, Pubkey);

    fn next(&mut self) -> Option<Self::Item> {
        // Retrieve the next record from the LMDB iterator.
        // The iterator returns `(&[u8], &[u8])` representing (key, value).
        let (_, value_bytes) = self.iter.next()?.ok()?;

        // Unpack the value which contains the Offset and Pubkey.
        // Usage matches the `programs` index layout: [Offset (4 bytes) | Pubkey (32 bytes)]
        Some(bytes!(#unpack, value_bytes, Offset, Pubkey))
    }
}
