use lmdb::{Cursor, RoCursor, RoTransaction};
use log::error;
use solana_pubkey::Pubkey;

use super::{table::Table, MDB_SET_OP};
use crate::{index::Offset, AccountsDbResult};

/// Iterator over pubkeys and offsets, where accounts
/// for those pubkeys can be found in the database
pub struct OffsetPubkeyIter<'env> {
    iter: lmdb::Iter<'env>,
    _cursor: RoCursor<'env>,
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
        // nasty/neat trick for lifetime erasure, but we are upholding
        // the rust's ownership contracts by keeping txn around as well
        let mut cursor: RoCursor = unsafe { std::mem::transmute(cursor) };
        let iter = if let Some(pubkey) = pubkey {
            // NOTE: there's a bug in the LMDB, which ignores NotFound error when
            // iterating on DUPSORT databases, where it just jumps to the next key,
            // here we check for the error explicitly to prevent this behavior
            if let Err(lmdb::Error::NotFound) =
                cursor.get(Some(pubkey.as_ref()), None, MDB_SET_OP)
            {
                lmdb::Iter::Err(lmdb::Error::NotFound)
            } else {
                cursor.iter_dup_of(pubkey)
            }
        } else {
            cursor.iter_start()
        };
        Ok(Self {
            _txn: txn,
            _cursor: cursor,
            iter,
        })
    }
}

impl Iterator for OffsetPubkeyIter<'_> {
    type Item = (Offset, Pubkey);
    fn next(&mut self) -> Option<Self::Item> {
        match self.iter.next()? {
            Ok(entry) => Some(bytes!(#unpack, entry.1, Offset, Pubkey)),
            Err(error) => {
                error!("error advancing offset iterator cursor: {error}");
                None
            }
        }
    }
}
