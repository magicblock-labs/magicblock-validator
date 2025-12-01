use std::{
    fs,
    path::Path,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
};

use rocksdb::{
    AsColumnFamilyRef, CStrLike, ColumnFamily, DBIterator, DBPinnableSlice,
    DBRawIterator, FlushOptions, IteratorMode as RocksIteratorMode, LiveFile,
    Options, WriteBatch as RWriteBatch, DB,
};
use solana_sdk::clock::Slot;

use super::{
    cf_descriptors::cf_descriptors,
    columns::Column,
    iterator::IteratorMode,
    options::{AccessType, LedgerOptions},
    rocksdb_options::get_rocksdb_options,
};
use crate::errors::{LedgerError, LedgerResult};

// -----------------
// Rocks
// -----------------
#[derive(Debug)]
pub struct Rocks {
    pub db: DB,
    access_type: AccessType,
    /// Oldest slot we want to keep in DB, slots before will be removed
    oldest_slot: Arc<AtomicU64>,
}

impl Rocks {
    pub fn open(path: &Path, options: LedgerOptions) -> LedgerResult<Self> {
        const DEFAULT_OLD_SLOT: Slot = 0;

        let access_type = options.access_type.clone();
        fs::create_dir_all(path)?;

        let oldest_slot = Arc::new(DEFAULT_OLD_SLOT.into());
        let db_options = get_rocksdb_options(&access_type);
        let descriptors = cf_descriptors(path, &options, &oldest_slot);

        let db = match access_type {
            AccessType::Primary => {
                DB::open_cf_descriptors(&db_options, path, descriptors)?
            }
            _ => unreachable!("Only primary access is supported"),
        };

        Ok(Self {
            db,
            access_type,
            oldest_slot,
        })
    }

    pub fn destroy(path: &Path) -> LedgerResult<()> {
        DB::destroy(&Options::default(), path)?;

        Ok(())
    }

    pub fn cf_handle(&self, cf: &str) -> &ColumnFamily {
        self.db
            .cf_handle(cf)
            .expect("should never get an unknown column")
    }

    pub fn get_cf(
        &self,
        cf: &ColumnFamily,
        key: &[u8],
    ) -> LedgerResult<Option<Vec<u8>>> {
        let opt = self.db.get_cf(cf, key)?;
        Ok(opt)
    }

    pub fn get_pinned_cf(
        &self,
        cf: &ColumnFamily,
        key: &[u8],
    ) -> LedgerResult<Option<DBPinnableSlice>> {
        let opt = self.db.get_pinned_cf(cf, key)?;
        Ok(opt)
    }

    pub fn put_cf(
        &self,
        cf: &ColumnFamily,
        key: &[u8],
        value: &[u8],
    ) -> LedgerResult<()> {
        self.db.put_cf(cf, key, value)?;
        Ok(())
    }

    pub fn multi_get_cf(
        &self,
        cf: &ColumnFamily,
        keys: Vec<&[u8]>,
    ) -> Vec<LedgerResult<Option<DBPinnableSlice>>> {
        let values = self
            .db
            .batched_multi_get_cf(cf, keys, false)
            .into_iter()
            .map(|result| match result {
                Ok(opt) => Ok(opt),
                Err(e) => Err(LedgerError::RocksDb(e)),
            })
            .collect::<Vec<_>>();
        values
    }

    pub fn delete_cf(&self, cf: &ColumnFamily, key: &[u8]) -> LedgerResult<()> {
        self.db.delete_cf(cf, key)?;
        Ok(())
    }

    /// Delete files whose slot range is within \[`from`, `to`\].
    pub fn delete_file_in_range_cf(
        &self,
        cf: &ColumnFamily,
        from_key: &[u8],
        to_key: &[u8],
    ) -> LedgerResult<()> {
        self.db.delete_file_in_range_cf(cf, from_key, to_key)?;
        Ok(())
    }

    /// Compacts keys in range \[`from`, `to`\].
    /// For leveled compaction style, all files containing keys in the given range
    /// are compacted to the last level containing files.
    /// https://github.com/facebook/rocksdb/wiki/Manual-Compaction#compactrange
    pub fn compact_range_cf<S: AsRef<[u8]>, E: AsRef<[u8]>>(
        &self,
        cf: &ColumnFamily,
        from_key: Option<S>,
        to_key: Option<E>,
    ) {
        self.db.compact_range_cf(cf, from_key, to_key)
    }

    /// Flushes column family
    pub fn flush_cf(&self, cf: &ColumnFamily) -> LedgerResult<()> {
        Ok(self.db.flush_cf(cf)?)
    }

    /// Flushed column families
    pub fn flush_cfs_opt(
        &self,
        cfs: &[&impl AsColumnFamilyRef],
        options: &FlushOptions,
    ) -> LedgerResult<()> {
        Ok(self.db.flush_cfs_opt(cfs, options)?)
    }

    pub fn iterator_cf<C>(
        &self,
        cf: &ColumnFamily,
        iterator_mode: IteratorMode<C::Index>,
    ) -> DBIterator
    where
        C: Column,
    {
        let start_key;
        let iterator_mode = match iterator_mode {
            IteratorMode::From(start_from, direction) => {
                start_key = C::key(start_from);
                RocksIteratorMode::From(&start_key, direction)
            }
            IteratorMode::Start => RocksIteratorMode::Start,
            IteratorMode::End => RocksIteratorMode::End,
        };
        self.db.iterator_cf(cf, iterator_mode)
    }

    pub fn iterator_cf_raw_key(
        &self,
        cf: &ColumnFamily,
        iterator_mode: IteratorMode<Vec<u8>>,
    ) -> DBIterator {
        let start_key;
        let iterator_mode = match iterator_mode {
            IteratorMode::From(start_from, direction) => {
                start_key = start_from;
                RocksIteratorMode::From(&start_key, direction)
            }
            IteratorMode::Start => RocksIteratorMode::Start,
            IteratorMode::End => RocksIteratorMode::End,
        };
        self.db.iterator_cf(cf, iterator_mode)
    }

    pub fn raw_iterator_cf(&self, cf: &ColumnFamily) -> DBRawIterator {
        self.db.raw_iterator_cf(cf)
    }

    pub fn batch(&self) -> RWriteBatch {
        RWriteBatch::default()
    }

    pub fn write(&self, batch: RWriteBatch) -> LedgerResult<()> {
        // let op_start_instant = maybe_enable_rocksdb_perf(
        //     self.column_options.rocks_perf_sample_interval,
        //     &self.write_batch_perf_status,
        // );
        let result = self.db.write(batch);
        // if let Some(op_start_instant) = op_start_instant {
        //     report_rocksdb_write_perf(
        //         PERF_METRIC_OP_NAME_WRITE_BATCH, // We use write_batch as cf_name for write batch.
        //         PERF_METRIC_OP_NAME_WRITE_BATCH, // op_name
        //         &op_start_instant.elapsed(),
        //         &self.column_options,
        //     );
        // }
        match result {
            Ok(_) => Ok(()),
            Err(e) => Err(LedgerError::RocksDb(e)),
        }
    }

    pub fn is_primary_access(&self) -> bool {
        self.access_type == AccessType::Primary
            || self.access_type == AccessType::PrimaryForMaintenance
    }

    /// Retrieves the specified RocksDB integer property of the current
    /// column family.
    ///
    /// Full list of properties that return int values could be found
    /// [here](https://github.com/facebook/rocksdb/blob/08809f5e6cd9cc4bc3958dd4d59457ae78c76660/include/rocksdb/db.h#L654-L689).
    pub fn get_int_property_cf(
        &self,
        cf: &ColumnFamily,
        name: impl CStrLike,
    ) -> LedgerResult<i64> {
        match self.db.property_int_value_cf(cf, name) {
            Ok(Some(value)) => Ok(value.try_into().unwrap()),
            Ok(None) => Ok(0),
            Err(e) => Err(LedgerError::RocksDb(e)),
        }
    }

    pub fn live_files_metadata(&self) -> LedgerResult<Vec<LiveFile>> {
        match self.db.live_files() {
            Ok(live_files) => Ok(live_files),
            Err(e) => Err(LedgerError::RocksDb(e)),
        }
    }

    /// Stores oldest maintained slot in db
    /// Used in CompactionFilter to decide if slot can be safely removed
    pub fn set_oldest_slot(&self, slot: Slot) {
        self.oldest_slot.store(slot, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use rocksdb::Options;
    use tempfile::tempdir;

    use super::*;
    use crate::database::columns::columns;

    #[test]
    fn test_cf_names_and_descriptors_equal_length() {
        let path = PathBuf::default();
        let options = LedgerOptions::default();
        // The names and descriptors don't need to be in the same order for our use cases;
        // however, there should be the same number of each. For example, adding a new column
        // should update both lists.
        assert_eq!(
            columns().len(),
            cf_descriptors(&path, &options, &Arc::new(0.into())).len()
        );
    }

    #[test]
    fn test_open_unknown_columns() {
        let temp_dir = tempdir().unwrap();
        let db_path = temp_dir.path();

        // Open with Primary to create the new database
        {
            let options = LedgerOptions {
                access_type: AccessType::Primary,
                ..Default::default()
            };
            let mut rocks = Rocks::open(db_path, options).unwrap();

            // Introduce a new column that will not be known
            rocks
                .db
                .create_cf("new_column", &Options::default())
                .unwrap();
        }

        // Opening with either Secondary or Primary access should succeed,
        // even though the Rocks code is unaware of "new_column"
        {
            let options = LedgerOptions {
                access_type: AccessType::Primary,
                ..Default::default()
            };
            let _ = Rocks::open(db_path, options).unwrap();
        }
    }
}
