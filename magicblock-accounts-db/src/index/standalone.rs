use std::{
    ops::{Deref, DerefMut},
    path::Path,
};

use lmdb::{
    Cursor, Database, DatabaseFlags, Environment, RoTransaction, RwCursor,
    RwTransaction, Transaction,
};

use super::{
    lmdb_utils::{lmdb_env, MDB_SET_OP},
    WEMPTY,
};
use crate::{log_err, AdbResult};

pub(super) struct StandaloneIndex {
    db: Database,
    env: Environment,
}

impl StandaloneIndex {
    pub(super) fn new(
        name: &str,
        dbpath: &Path,
        size: usize,
        flags: DatabaseFlags,
    ) -> AdbResult<Self> {
        let env = lmdb_env(name, dbpath, size, 1).inspect_err(log_err!(
            "deallocation index creation at {}",
            dbpath.display()
        ))?;
        let db = env.create_db(None, flags)?;
        Ok(Self { env, db })
    }

    pub(super) fn put(
        &self,
        key: impl AsRef<[u8]>,
        val: impl AsRef<[u8]>,
    ) -> lmdb::Result<()> {
        let mut txn = self.rwtxn()?;
        txn.put(self.db, &key, &val, WEMPTY)?;
        txn.commit()
    }

    pub(super) fn getter(&self) -> lmdb::Result<StandaloneIndexGetter> {
        self.rotxn()
            .map(|txn| StandaloneIndexGetter { txn, db: self.db })
    }

    pub(super) fn del(&self, key: impl AsRef<[u8]>) -> lmdb::Result<()> {
        let mut txn = self.rwtxn()?;
        let mut cursor = txn.open_rw_cursor(self.db)?;
        match cursor.get(Some(key.as_ref()), None, MDB_SET_OP) {
            Ok(_) => (),
            Err(lmdb::Error::NotFound) => return Ok(()),
            Err(err) => Err(err)?,
        }
        cursor.del(WEMPTY)?;
        drop(cursor);
        txn.commit()
    }

    pub(super) fn cursor(&self) -> lmdb::Result<StandaloneIndexCursor<'_>> {
        let mut txn = self.rwtxn()?;
        let inner = txn.open_rw_cursor(self.db)?;
        // SAFETY:
        // We erase the lifetime of cursor which is bound to _txn since we keep
        // txn bundled with cursor (inner) it's safe to perform the transmutation
        let inner = unsafe {
            std::mem::transmute::<lmdb::RwCursor<'_>, lmdb::RwCursor<'_>>(inner)
        };

        Ok(StandaloneIndexCursor { inner, txn })
    }

    pub(super) fn sync(&self) {
        // it's ok to ignore error, as it will only happen if something utterly terrible
        // happened at OS level, in which case we most likely won't even reach this code
        let _ = self
            .env
            .sync(true)
            .inspect_err(log_err!("secondary index flushing"));
    }

    pub(super) fn len(&self) -> usize {
        self.env
            .stat()
            .inspect_err(log_err!("secondary index stat retrieval"))
            .map(|stat| stat.entries())
            .unwrap_or_default()
    }

    fn rotxn(&self) -> lmdb::Result<RoTransaction> {
        self.env.begin_ro_txn()
    }

    fn rwtxn(&self) -> lmdb::Result<RwTransaction> {
        self.env.begin_rw_txn()
    }
}

pub(super) struct StandaloneIndexGetter<'a> {
    txn: RoTransaction<'a>,
    db: Database,
}

impl StandaloneIndexGetter<'_> {
    pub(super) fn get(&self, key: impl AsRef<[u8]>) -> lmdb::Result<&[u8]> {
        self.txn.get(self.db, &key)
    }
}

pub(super) struct StandaloneIndexCursor<'a> {
    inner: RwCursor<'a>,
    txn: RwTransaction<'a>,
}

impl<'a> Deref for StandaloneIndexCursor<'a> {
    type Target = RwCursor<'a>;
    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl DerefMut for StandaloneIndexCursor<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}

impl StandaloneIndexCursor<'_> {
    pub(super) fn commit(self) -> lmdb::Result<()> {
        drop(self.inner);
        self.txn.commit()
    }
}
