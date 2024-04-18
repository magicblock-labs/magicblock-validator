use bincode::deserialize;
use rocksdb::{ColumnFamily, DBRawIterator, LiveFile};
use solana_sdk::clock::Slot;
use std::{path::Path, sync::Arc};

use super::{
    columns::{columns, Column, ColumnName, TypedColumn},
    errors::BlockstoreResult,
    iterator::IteratorMode,
    options::BlockstoreOptions,
    rocks_db::Rocks,
    write_batch::WriteBatch,
};

#[derive(Debug)]
pub struct Database {
    backend: Arc<Rocks>,
    path: Arc<Path>,
    // column_options: Arc<LedgerColumnOptions>,
}

impl Database {
    pub fn open(
        path: &Path,
        options: BlockstoreOptions,
    ) -> BlockstoreResult<Self> {
        // let column_options = Arc::new(options.column_options.clone());
        let backend = Arc::new(Rocks::open(path, options)?);

        Ok(Database {
            backend,
            path: Arc::from(path),
            // column_options,
        })
    }

    pub fn destroy(path: &Path) -> BlockstoreResult<()> {
        Rocks::destroy(path)?;

        Ok(())
    }

    pub fn get<C>(&self, key: C::Index) -> BlockstoreResult<Option<C::Type>>
    where
        C: TypedColumn + ColumnName,
    {
        if let Some(pinnable_slice) = self
            .backend
            .get_pinned_cf(self.cf_handle::<C>(), &C::key(key))?
        {
            let value = deserialize(pinnable_slice.as_ref())?;
            Ok(Some(value))
        } else {
            Ok(None)
        }
    }

    pub fn iter<C>(
        &self,
        iterator_mode: IteratorMode<C::Index>,
    ) -> BlockstoreResult<impl Iterator<Item = (C::Index, Box<[u8]>)> + '_>
    where
        C: Column + ColumnName,
    {
        let cf = self.cf_handle::<C>();
        let iter = self.backend.iterator_cf::<C>(cf, iterator_mode);
        Ok(iter.map(|pair| {
            let (key, value) = pair.unwrap();
            (C::index(&key), value)
        }))
    }

    #[inline]
    pub fn cf_handle<C: ColumnName>(&self) -> &ColumnFamily
    where
        C: Column + ColumnName,
    {
        self.backend.cf_handle(C::NAME)
    }

    #[inline]
    pub fn raw_iterator_cf(
        &self,
        cf: &ColumnFamily,
    ) -> BlockstoreResult<DBRawIterator> {
        Ok(self.backend.raw_iterator_cf(cf))
    }

    pub fn batch(&self) -> BlockstoreResult<WriteBatch> {
        let write_batch = self.backend.batch();
        let map = columns()
            .into_iter()
            .map(|desc| (desc, self.backend.cf_handle(desc)))
            .collect();

        Ok(WriteBatch { write_batch, map })
    }

    pub fn write(&self, batch: WriteBatch) -> BlockstoreResult<()> {
        self.backend.write(batch.write_batch)
    }

    pub fn storage_size(&self) -> BlockstoreResult<u64> {
        todo!("storage_size after adding fs_extra crate")
        // Ok(fs_extra::dir::get_size(&self.path)?)
    }

    /// Adds a \[`from`, `to`\] range that deletes all entries between the `from` slot
    /// and `to` slot inclusively.  If `from` slot and `to` slot are the same, then all
    /// entries in that slot will be removed.
    pub fn delete_range_cf<C>(
        &self,
        batch: &mut WriteBatch,
        from: Slot,
        to: Slot,
    ) -> BlockstoreResult<()>
    where
        C: Column + ColumnName,
    {
        let cf = self.cf_handle::<C>();
        // Note that the default behavior of rocksdb's delete_range_cf deletes
        // files within [from, to), while our purge logic applies to [from, to].
        //
        // For consistency, we make our delete_range_cf works for [from, to] by
        // adjusting the `to` slot range by 1.
        let from_index = C::as_index(from);
        let to_index = C::as_index(to.saturating_add(1));
        batch.delete_range_cf::<C>(cf, from_index, to_index)
    }

    /// Delete files whose slot range is within \[`from`, `to`\].
    pub fn delete_file_in_range_cf<C>(
        &self,
        from: Slot,
        to: Slot,
    ) -> BlockstoreResult<()>
    where
        C: Column + ColumnName,
    {
        self.backend.delete_file_in_range_cf(
            self.cf_handle::<C>(),
            &C::key(C::as_index(from)),
            &C::key(C::as_index(to)),
        )
    }

    pub fn is_primary_access(&self) -> bool {
        self.backend.is_primary_access()
    }

    pub fn live_files_metadata(&self) -> BlockstoreResult<Vec<LiveFile>> {
        self.backend.live_files_metadata()
    }

    pub fn compact_range_cf<C: Column + ColumnName>(
        &self,
        from: &[u8],
        to: &[u8],
    ) {
        let cf = self.cf_handle::<C>();
        self.backend.db.compact_range_cf(cf, Some(from), Some(to));
    }
}
