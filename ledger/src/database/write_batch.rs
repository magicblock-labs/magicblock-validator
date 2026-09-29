use std::collections::HashMap;

use bincode::serialize;
use rocksdb::{ColumnFamily, WriteBatch as RWriteBatch};

use super::columns::{Column, ColumnName, TypedColumn};
use crate::errors::LedgerError;

pub struct WriteBatch<'a> {
    pub write_batch: RWriteBatch,
    pub map: HashMap<&'static str, &'a ColumnFamily>,
}

impl<'a> WriteBatch<'a> {
    pub fn put_bytes<C: Column + ColumnName>(
        &mut self,
        key: C::Index,
        bytes: &[u8],
    ) {
        self.write_batch
            .put_cf(self.get_cf::<C>(), C::key(key), bytes);
    }

    pub fn delete<C: Column + ColumnName>(&mut self, key: C::Index) {
        self.delete_raw::<C>(&C::key(key));
    }

    pub(crate) fn delete_raw<C: Column + ColumnName>(&mut self, key: &[u8]) {
        self.write_batch.delete_cf(self.get_cf::<C>(), key);
    }

    pub fn put<C: TypedColumn + ColumnName>(
        &mut self,
        key: C::Index,
        value: &C::Type,
    ) -> Result<(), LedgerError> {
        let serialized_value = serialize(&value)?;
        self.write_batch.put_cf(
            self.get_cf::<C>(),
            C::key(key),
            serialized_value,
        );
        Ok(())
    }

    #[inline]
    pub fn get_cf<C: Column + ColumnName>(&self) -> &'a ColumnFamily {
        self.map[C::NAME]
    }

    /// Adds a \[`from`, `to`) range deletion entry to the batch.
    ///
    /// Note that the \[`from`, `to`) deletion range of WriteBatch::delete_range_cf
    /// is different from \[`from`, `to`\] of Database::delete_range_cf as we makes
    /// the semantics of Database::delete_range_cf matches the blockstore purge
    /// logic.
    pub fn delete_range_cf<C: Column>(
        &mut self,
        cf: &ColumnFamily,
        from: C::Index,
        to: C::Index, // exclusive
    ) {
        self.write_batch
            .delete_range_cf(cf, C::key(from), C::key(to));
    }
}
