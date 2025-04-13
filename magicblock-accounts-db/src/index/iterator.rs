use lmdb::{Cursor, Database, RoCursor, RoTransaction, Transaction};
use log::error;
use solana_pubkey::Pubkey;

use super::lmdb_utils::MDB_GET_CURRENT_OP;
use crate::AdbResult;

/// Iterator over pubkeys and offsets, where accounts
/// for those pubkeys can be found in database
///
/// S: Starting position operation, determines where to place cursor initially
/// N: Next position operation, determines where to move cursor next
pub(crate) struct OffsetPubkeyIter<'env, const S: u32, const N: u32> {
    cursor: RoCursor<'env>,
    terminated: bool,
    _txn: RoTransaction<'env>,
}

impl<'a, const S: u32, const N: u32> OffsetPubkeyIter<'a, S, N> {
    pub(super) fn new(
        db: Database,
        txn: RoTransaction<'a>,
        pubkey: Option<&Pubkey>,
    ) -> AdbResult<Self> {
        let cursor = txn.open_ro_cursor(db)?;
        // SAFETY:
        // nasty/neat trick for lifetime erasure, but we are upholding
        // the rust's  ownership contracts by keeping txn around as well
        let cursor: RoCursor = unsafe { std::mem::transmute(cursor) };
        // jump to the first entry, key might be ignored depending on OP
        cursor.get(pubkey.map(AsRef::as_ref), None, S)?;
        Ok(Self {
            _txn: txn,
            cursor,
            terminated: false,
        })
    }
}

impl<const S: u32, const N: u32> Iterator for OffsetPubkeyIter<'_, S, N> {
    type Item = (u32, Pubkey);
    fn next(&mut self) -> Option<Self::Item> {
        if self.terminated {
            return None;
        }

        match self.cursor.get(None, None, MDB_GET_CURRENT_OP) {
            Ok(entry) => {
                // advance the cursor,
                let advance = self.cursor.get(None, None, N);
                // if we move past the iterable range, NotFound will be
                // triggered by OP, and we can terminate the iteration
                if let Err(lmdb::Error::NotFound) = advance {
                    self.terminated = true;
                }
                Some(bytes!(#unpack, entry.1, u32, Pubkey))
            }
            Err(error) => {
                error!("error advancing offset iterator cursor: {error}");
                None
            }
        }
    }
}
