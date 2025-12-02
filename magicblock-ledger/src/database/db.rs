use std::{
    marker::PhantomData,
    path::Path,
    sync::{atomic::AtomicI64, Arc},
};

use bincode::deserialize;
use rocksdb::{ColumnFamily, DBRawIterator, LiveFile};
use solana_sdk::clock::Slot;

use super::{
    columns::{columns, Column, ColumnName, TypedColumn},
    iterator::IteratorMode,
    ledger_column::LedgerColumn,
    options::{LedgerColumnOptions, LedgerOptions},
    rocks_db::Rocks,
    write_batch::WriteBatch,
};
use crate::{
    database::columns::DIRTY_COUNT, errors::LedgerError,
    metrics::PerfSamplingStatus,
};

#[derive(Debug)]
pub struct Database {
    pub(crate) backend: Arc<Rocks>,
    path: Arc<Path>,
    column_options: Arc<LedgerColumnOptions>,
}

impl Database {
    pub fn open(
        path: &Path,
        options: LedgerOptions,
    ) -> Result<Self, LedgerError> {
        let column_options = Arc::new(options.column_options.clone());
        let backend = Arc::new(Rocks::open(path, options)?);

        Ok(Database {
            backend,
            path: Arc::from(path),
            column_options,
        })
    }

    pub fn destroy(path: &Path) -> Result<(), LedgerError> {
        Rocks::destroy(path)?;

        Ok(())
    }

    pub fn get<C>(&self, key: C::Index) -> Result<Option<C::Type>, LedgerError>
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
    ) -> Result<impl Iterator<Item = (C::Index, Box<[u8]>)> + '_, LedgerError>
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
    pub fn cf_handle<C>(&self) -> &ColumnFamily
    where
        C: Column + ColumnName,
    {
        self.backend.cf_handle(C::NAME)
    }

    pub fn column<C>(&self) -> LedgerColumn<C>
    where
        C: Column + ColumnName,
    {
        LedgerColumn {
            backend: Arc::clone(&self.backend),
            column: PhantomData,
            column_options: Arc::clone(&self.column_options),
            read_perf_status: PerfSamplingStatus::default(),
            write_perf_status: PerfSamplingStatus::default(),
            entry_counter: AtomicI64::new(DIRTY_COUNT),
        }
    }

    #[inline]
    pub fn raw_iterator_cf(&self, cf: &ColumnFamily) -> DBRawIterator<'_> {
        self.backend.raw_iterator_cf(cf)
    }

    pub fn batch(&self) -> WriteBatch<'_> {
        let write_batch = self.backend.batch();
        let map = columns()
            .into_iter()
            .map(|desc| (desc, self.backend.cf_handle(desc)))
            .collect();

        WriteBatch { write_batch, map }
    }

    pub fn write(&self, batch: WriteBatch) -> Result<(), LedgerError> {
        self.backend.write(batch.write_batch)
    }

    pub fn storage_size(&self) -> Result<u64, LedgerError> {
        Ok(fs_extra::dir::get_size(&self.path)?)
    }

    /// Adds a \[`from`, `to`\] range that deletes all entries between the `from` slot
    /// and `to` slot inclusively.  If `from` slot and `to` slot are the same, then all
    /// entries in that slot will be removed.
    pub fn delete_range_cf<C>(
        &self,
        batch: &mut WriteBatch,
        from: Slot,
        to: Slot,
    ) where
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
        batch.delete_range_cf::<C>(cf, from_index, to_index);
    }

    /// Delete files whose slot range is within \[`from`, `to`\].
    pub fn delete_file_in_range_cf<C>(
        &self,
        from: Slot,
        to: Slot,
    ) -> std::result::Result<(), LedgerError>
    where
        C: Column + ColumnName,
    {
        self.backend.delete_file_in_range_cf(
            self.cf_handle::<C>(),
            &C::key(C::as_index(from)),
            &C::key(C::as_index(to)),
        )
    }

    /// See [crate::database::rocks_db::Rocks::compact_range_cf] for documentation.
    pub fn compact_range_cf<C>(
        &self,
        from: Option<C::Index>,
        to: Option<C::Index>,
    ) where
        C: Column + ColumnName,
    {
        self.backend.compact_range_cf(
            self.cf_handle::<C>(),
            from.map(|index| C::key(index)),
            to.map(|index| C::key(index)),
        )
    }

    pub fn is_primary_access(&self) -> bool {
        self.backend.is_primary_access()
    }

    pub fn live_files_metadata(
        &self,
    ) -> std::result::Result<Vec<LiveFile>, LedgerError> {
        self.backend.live_files_metadata()
    }

    /// Stores oldest maintained slot in db
    /// Used in CompactionFilter to decide if slot can be safely removed
    pub fn set_oldest_slot(&self, slot: Slot) {
        self.backend.set_oldest_slot(slot);
    }
}
