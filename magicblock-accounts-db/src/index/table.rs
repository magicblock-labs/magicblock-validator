use lmdb::{
    Database, DatabaseFlags, Environment, RoCursor, RwCursor, RwTransaction,
    Transaction, WriteFlags,
};

/// Wrapper around LMDB Database providing a safe, typed interface.
#[cfg_attr(test, derive(Debug))]
pub struct Table {
    pub db: Database,
}

impl Table {
    /// Opens or creates a database within the given environment.
    pub(super) fn new(
        env: &Environment,
        name: &str,
        flags: DatabaseFlags,
    ) -> lmdb::Result<Self> {
        let db = env.create_db(Some(name), flags)?;
        Ok(Self { db })
    }

    /// Retrieves a value by key. Returns `None` if the key does not exist.
    #[inline]
    pub fn get<'txn, T: Transaction, K: AsRef<[u8]>>(
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

    /// Inserts a key-value pair, **overwriting** any existing value.
    #[inline]
    pub(super) fn upsert<K: AsRef<[u8]>, V: AsRef<[u8]>>(
        &self,
        txn: &mut RwTransaction,
        key: K,
        value: V,
    ) -> lmdb::Result<()> {
        txn.put(self.db, &key, &value, WriteFlags::empty())
    }

    /// Inserts a key-value pair **only if the key does not already exist**.
    /// Returns `true` if inserted, `false` if the key already existed.
    #[inline]
    pub fn insert<K: AsRef<[u8]>, V: AsRef<[u8]>>(
        &self,
        txn: &mut RwTransaction,
        key: K,
        value: V,
    ) -> lmdb::Result<bool> {
        match txn.put(self.db, &key, &value, WriteFlags::NO_OVERWRITE) {
            Ok(_) => Ok(true),
            Err(lmdb::Error::KeyExist) => Ok(false),
            Err(e) => Err(e),
        }
    }

    /// Removes a key. Returns `Ok(())` even if the key was not found (idempotent).
    ///
    /// If `value` is provided, the deletion only occurs if the data in the DB matches.
    #[inline]
    pub fn remove<K: AsRef<[u8]>>(
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

    /// Opens a read-only cursor.
    #[inline]
    pub fn cursor_ro<'txn, T: Transaction>(
        &self,
        txn: &'txn T,
    ) -> lmdb::Result<RoCursor<'txn>> {
        txn.open_ro_cursor(self.db)
    }

    /// Opens a read-write cursor.
    #[inline]
    pub fn cursor_rw<'txn>(
        &self,
        txn: &'txn mut RwTransaction,
    ) -> lmdb::Result<RwCursor<'txn>> {
        txn.open_rw_cursor(self.db)
    }

    /// Returns the number of entries in the database.
    pub fn entries<T: Transaction>(&self, txn: &T) -> usize {
        txn.stat(self.db).map(|s| s.entries()).unwrap_or_default()
    }
}
