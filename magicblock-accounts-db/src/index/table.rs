use lmdb::{
    Database, DatabaseFlags, Environment, RoCursor, RwCursor, RwTransaction,
    Transaction, WriteFlags,
};

use super::WEMPTY;

#[cfg_attr(test, derive(Debug))]
pub(super) struct Table {
    db: Database,
}

impl Table {
    pub(super) fn new(
        env: &Environment,
        name: &str,
        flags: DatabaseFlags,
    ) -> lmdb::Result<Self> {
        let db = env.create_db(Some(name), flags)?;
        Ok(Self { db })
    }

    #[inline]
    pub(super) fn get<'txn, T: Transaction, K: AsRef<[u8]>>(
        &self,
        txn: &'txn T,
        key: K,
    ) -> lmdb::Result<Option<&'txn [u8]>> {
        match txn.get(self.db, &key) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(lmdb::Error::NotFound) => Ok(None),
            Err(e) => Err(e),
        }
    }

    #[inline]
    pub(super) fn put<K: AsRef<[u8]>, V: AsRef<[u8]>>(
        &self,
        txn: &mut RwTransaction,
        key: K,
        value: V,
    ) -> lmdb::Result<()> {
        txn.put(self.db, &key, &value, WEMPTY)
    }

    #[inline]
    pub(super) fn del<K: AsRef<[u8]>>(
        &self,
        txn: &mut RwTransaction,
        key: K,
        value: Option<&[u8]>,
    ) -> lmdb::Result<()> {
        match txn.del(self.db, &key, value) {
            Ok(_) | Err(lmdb::Error::NotFound) => Ok(()),
            Err(e) => Err(e),
        }
    }

    #[inline]
    pub(super) fn put_if_not_exists<K: AsRef<[u8]>, V: AsRef<[u8]>>(
        &self,
        txn: &mut RwTransaction,
        key: K,
        value: V,
    ) -> lmdb::Result<bool> {
        let result = txn.put(self.db, &key, &value, WriteFlags::NO_OVERWRITE);
        match result {
            Ok(_) => Ok(true),
            Err(lmdb::Error::KeyExist) => Ok(false),
            Err(err) => Err(err),
        }
    }

    #[inline]
    pub(super) fn cursor_ro<'txn, T: Transaction>(
        &self,
        txn: &'txn T,
    ) -> lmdb::Result<RoCursor<'txn>> {
        txn.open_ro_cursor(self.db)
    }

    #[inline]
    pub(super) fn cursor_rw<'txn>(
        &self,
        txn: &'txn mut RwTransaction,
    ) -> lmdb::Result<RwCursor<'txn>> {
        txn.open_rw_cursor(self.db)
    }

    pub(super) fn entries<T: Transaction>(&self, txn: &T) -> usize {
        txn.stat(self.db).map(|s| s.entries()).unwrap_or_default()
    }
}
