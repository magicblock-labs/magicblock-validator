use std::{
    marker::PhantomData,
    sync::{
        atomic::{AtomicI64, Ordering},
        Arc,
    },
};

use bincode::{deserialize, serialize};
use log::warn;
use prost::Message;
use rocksdb::{properties as RocksProperties, CStrLike, ColumnFamily};
use serde::de::DeserializeOwned;

use super::{
    columns::{
        Column, ColumnIndexDeprecation, ColumnName, ProtobufColumn, TypedColumn,
    },
    iterator::IteratorMode,
    options::LedgerColumnOptions,
    rocks_db::Rocks,
};
use crate::{
    database::{columns::DIRTY_COUNT, write_batch::WriteBatch},
    errors::{LedgerError, LedgerResult},
    metrics::{
        maybe_enable_rocksdb_perf, report_rocksdb_read_perf,
        report_rocksdb_write_perf, BlockstoreRocksDbColumnFamilyMetrics,
        PerfSamplingStatus, BLOCKSTORE_METRICS_ERROR, PERF_METRIC_OP_NAME_GET,
        PERF_METRIC_OP_NAME_MULTI_GET, PERF_METRIC_OP_NAME_PUT,
    },
};

#[derive(Debug)]
pub struct LedgerColumn<C>
where
    C: Column + ColumnName,
{
    pub backend: Arc<Rocks>,
    pub column: PhantomData<C>,
    pub column_options: Arc<LedgerColumnOptions>,
    pub read_perf_status: PerfSamplingStatus,
    pub write_perf_status: PerfSamplingStatus,
    // We are caching the column item counts since they are expensive to obtain.
    // `-1` indicates that they are "dirty"    //
    //     // We are using an i64 to make this work even though the counts are usize,
    //     // however if we had 50,000 transactions/sec and 50ms slots for 100 years then:
    //     //
    //     // slots:   200 * 3600 * 24 * 365 * 100 =           630,720,000,000
    //     // txs:  50,000 * 3600 * 24 * 365 * 100 =       157,680,000,000,000
    //     // i64::MAX                             = 9,223,372,036,854,775,807
    pub entry_counter: AtomicI64,
}

impl<C: Column + ColumnName> LedgerColumn<C> {
    pub fn submit_rocksdb_cf_metrics(&self) {
        let cf_rocksdb_metrics = BlockstoreRocksDbColumnFamilyMetrics {
            total_sst_files_size: self
                .get_int_property(RocksProperties::TOTAL_SST_FILES_SIZE)
                .unwrap_or(BLOCKSTORE_METRICS_ERROR),
            size_all_mem_tables: self
                .get_int_property(RocksProperties::SIZE_ALL_MEM_TABLES)
                .unwrap_or(BLOCKSTORE_METRICS_ERROR),
            num_snapshots: self
                .get_int_property(RocksProperties::NUM_SNAPSHOTS)
                .unwrap_or(BLOCKSTORE_METRICS_ERROR),
            oldest_snapshot_time: self
                .get_int_property(RocksProperties::OLDEST_SNAPSHOT_TIME)
                .unwrap_or(BLOCKSTORE_METRICS_ERROR),
            actual_delayed_write_rate: self
                .get_int_property(RocksProperties::ACTUAL_DELAYED_WRITE_RATE)
                .unwrap_or(BLOCKSTORE_METRICS_ERROR),
            is_write_stopped: self
                .get_int_property(RocksProperties::IS_WRITE_STOPPED)
                .unwrap_or(BLOCKSTORE_METRICS_ERROR),
            block_cache_capacity: self
                .get_int_property(RocksProperties::BLOCK_CACHE_CAPACITY)
                .unwrap_or(BLOCKSTORE_METRICS_ERROR),
            block_cache_usage: self
                .get_int_property(RocksProperties::BLOCK_CACHE_USAGE)
                .unwrap_or(BLOCKSTORE_METRICS_ERROR),
            block_cache_pinned_usage: self
                .get_int_property(RocksProperties::BLOCK_CACHE_PINNED_USAGE)
                .unwrap_or(BLOCKSTORE_METRICS_ERROR),
            estimate_table_readers_mem: self
                .get_int_property(RocksProperties::ESTIMATE_TABLE_READERS_MEM)
                .unwrap_or(BLOCKSTORE_METRICS_ERROR),
            mem_table_flush_pending: self
                .get_int_property(RocksProperties::MEM_TABLE_FLUSH_PENDING)
                .unwrap_or(BLOCKSTORE_METRICS_ERROR),
            compaction_pending: self
                .get_int_property(RocksProperties::COMPACTION_PENDING)
                .unwrap_or(BLOCKSTORE_METRICS_ERROR),
            num_running_compactions: self
                .get_int_property(RocksProperties::NUM_RUNNING_COMPACTIONS)
                .unwrap_or(BLOCKSTORE_METRICS_ERROR),
            num_running_flushes: self
                .get_int_property(RocksProperties::NUM_RUNNING_FLUSHES)
                .unwrap_or(BLOCKSTORE_METRICS_ERROR),
            estimate_oldest_key_time: self
                .get_int_property(RocksProperties::ESTIMATE_OLDEST_KEY_TIME)
                .unwrap_or(BLOCKSTORE_METRICS_ERROR),
            background_errors: self
                .get_int_property(RocksProperties::BACKGROUND_ERRORS)
                .unwrap_or(BLOCKSTORE_METRICS_ERROR),
        };
        cf_rocksdb_metrics.report_metrics(C::NAME, &self.column_options);
    }
}

impl<C> LedgerColumn<C>
where
    C: Column + ColumnName,
{
    pub fn get_bytes(
        &self,
        key: C::Index,
    ) -> Result<Option<Vec<u8>>, LedgerError> {
        let is_perf_enabled = maybe_enable_rocksdb_perf(
            self.column_options.rocks_perf_sample_interval,
            &self.read_perf_status,
        );
        let result = self.backend.get_cf(self.handle(), &C::key(key));
        if let Some(op_start_instant) = is_perf_enabled {
            report_rocksdb_read_perf(
                C::NAME,
                PERF_METRIC_OP_NAME_GET,
                &op_start_instant.elapsed(),
                &self.column_options,
            );
        }
        result
    }

    pub fn multi_get_bytes(
        &self,
        keys: Vec<C::Index>,
    ) -> Vec<std::result::Result<Option<Vec<u8>>, LedgerError>> {
        let rocks_keys: Vec<_> =
            keys.into_iter().map(|key| C::key(key)).collect();
        {
            let ref_rocks_keys: Vec<_> =
                rocks_keys.iter().map(|k| &k[..]).collect();
            let is_perf_enabled = maybe_enable_rocksdb_perf(
                self.column_options.rocks_perf_sample_interval,
                &self.read_perf_status,
            );
            let result = self
                .backend
                .multi_get_cf(self.handle(), ref_rocks_keys)
                .into_iter()
                .map(|r| match r {
                    Ok(opt) => match opt {
                        Some(pinnable_slice) => {
                            Ok(Some(pinnable_slice.as_ref().to_vec()))
                        }
                        None => Ok(None),
                    },
                    Err(e) => Err(e),
                })
                .collect::<Vec<std::result::Result<Option<_>, LedgerError>>>();
            if let Some(op_start_instant) = is_perf_enabled {
                // use multi-get instead
                report_rocksdb_read_perf(
                    C::NAME,
                    PERF_METRIC_OP_NAME_MULTI_GET,
                    &op_start_instant.elapsed(),
                    &self.column_options,
                );
            }

            result
        }
    }

    pub fn iter(
        &self,
        iterator_mode: IteratorMode<C::Index>,
    ) -> std::result::Result<
        impl Iterator<Item = (C::Index, Box<[u8]>)> + '_,
        LedgerError,
    > {
        let cf = self.handle();
        let iter = self.backend.iterator_cf::<C>(cf, iterator_mode);
        Ok(iter.map(|pair| {
            let (key, value) = pair.unwrap();
            (C::index(&key), value)
        }))
    }

    #[inline]
    pub fn handle(&self) -> &ColumnFamily {
        self.backend.cf_handle(C::NAME)
    }

    #[cfg(test)]
    pub fn is_empty(&self) -> std::result::Result<bool, LedgerError> {
        let mut iter = self.backend.raw_iterator_cf(self.handle());
        iter.seek_to_first();
        Ok(!iter.valid())
    }

    pub fn put_bytes(
        &self,
        key: C::Index,
        value: &[u8],
    ) -> std::result::Result<(), LedgerError> {
        let is_perf_enabled = maybe_enable_rocksdb_perf(
            self.column_options.rocks_perf_sample_interval,
            &self.write_perf_status,
        );
        let result = self.backend.put_cf(self.handle(), &C::key(key), value);
        if let Some(op_start_instant) = is_perf_enabled {
            report_rocksdb_write_perf(
                C::NAME,
                PERF_METRIC_OP_NAME_PUT,
                &op_start_instant.elapsed(),
                &self.column_options,
            );
        }
        result
    }

    /// Retrieves the specified RocksDB integer property of the current
    /// column family.
    ///
    /// Full list of properties that return int values could be found
    /// [here](https://github.com/facebook/rocksdb/blob/08809f5e6cd9cc4bc3958dd4d59457ae78c76660/include/rocksdb/db.h#L654-L689).
    pub fn get_int_property(
        &self,
        name: impl CStrLike,
    ) -> Result<i64, LedgerError> {
        self.backend.get_int_property_cf(self.handle(), name)
    }

    pub fn delete(&self, key: C::Index) -> Result<(), LedgerError> {
        let is_perf_enabled = maybe_enable_rocksdb_perf(
            self.column_options.rocks_perf_sample_interval,
            &self.write_perf_status,
        );
        let result = self.backend.delete_cf(self.handle(), &C::key(key));
        if let Some(op_start_instant) = is_perf_enabled {
            report_rocksdb_write_perf(
                C::NAME,
                "delete",
                &op_start_instant.elapsed(),
                &self.column_options,
            );
        }
        result
    }

    pub fn delete_in_batch(&self, write_batch: &mut WriteBatch, key: C::Index) {
        write_batch.delete::<C>(key);
    }

    pub fn delete_range(
        &self,
        from: C::Index,
        to: C::Index,
    ) -> LedgerResult<()> {
        self.backend
            .delete_range_cf(self.handle(), C::key(from), C::key(to))
    }

    pub fn delete_range_in_batch(
        &self,
        write_batch: &mut WriteBatch,
        from: C::Index,
        to: C::Index,
    ) {
        write_batch.delete_range_cf::<C>(self.handle(), from, to);
    }

    /// See [crate::database::rocks_db::Rocks::compact_range_cf] for documentation.
    pub fn compact_range(&self, from: Option<C::Index>, to: Option<C::Index>) {
        self.backend.compact_range_cf(
            self.handle(),
            from.map(|index| C::key(index)),
            to.map(|index| C::key(index)),
        )
    }

    /// See [crate::database::rocks_db::Rocks::flush_cf] for documentation.
    pub fn flush(&self) -> LedgerResult<()> {
        self.backend.flush_cf(self.handle())
    }

    #[inline(always)]
    fn is_sequential_cf<T: ColumnName>() -> bool {
        matches!(
            T::NAME,
            crate::database::columns::Blocktime::NAME
                | crate::database::columns::Blockhash::NAME
                | crate::database::columns::PerfSamples::NAME
                | crate::database::columns::AccountModDatas::NAME
        )
    }

    /// Initialize value when current one is `DIRTY_VALUE`
    /// Value isn't set if current doesn't equal `DIRTY_VALUE`
    fn init_column_count_cache(&self) -> LedgerResult<()> {
        let count = if Self::is_sequential_cf::<C>() {
            get_column_count_sequential_column(self)? as i64
        } else {
            get_column_count_complex_column(self)? as i64
        };

        // We can ignore error here since it means value already initialized
        let _ = self.entry_counter.compare_exchange(
            DIRTY_COUNT,
            count,
            Ordering::AcqRel,
            Ordering::Relaxed,
        );

        Ok(())
    }

    pub fn count_column_using_cache(&self) -> LedgerResult<i64> {
        let cached = self.entry_counter.load(Ordering::Relaxed);
        if cached != DIRTY_COUNT {
            Ok(cached)
        } else {
            self.init_column_count_cache()?;
            Ok(self.entry_counter.load(Ordering::Acquire))
        }
    }

    /// Increases entries counter if it's not [`DIRTY_COUNT`]
    /// Otherwise just skips it until it is set
    #[inline(always)]
    pub fn try_increase_entry_counter(&self, by: u64) {
        try_increase_entry_counter(&self.entry_counter, by);
    }

    /// Decreases entries counter if it's not [`DIRTY_COUNT`]
    /// Otherwise just skips it until it is set
    #[inline(always)]
    pub fn try_decrease_entry_counter(&self, by: u64) {
        try_decrease_entry_counter(&self.entry_counter, by);
    }
}

impl<C> LedgerColumn<C>
where
    C: TypedColumn + ColumnName,
{
    pub fn multi_get(
        &self,
        keys: Vec<C::Index>,
    ) -> Vec<std::result::Result<Option<C::Type>, LedgerError>> {
        let rocks_keys: Vec<_> =
            keys.into_iter().map(|key| C::key(key)).collect();
        {
            let ref_rocks_keys: Vec<_> =
                rocks_keys.iter().map(|k| &k[..]).collect();
            let is_perf_enabled = maybe_enable_rocksdb_perf(
                self.column_options.rocks_perf_sample_interval,
                &self.read_perf_status,
            );
            let result = self
                .backend
                .multi_get_cf(self.handle(), ref_rocks_keys)
                .into_iter()
                .map(|r| match r {
                    Ok(opt) => match opt {
                        Some(pinnable_slice) => {
                            Ok(Some(deserialize(pinnable_slice.as_ref())?))
                        }
                        None => Ok(None),
                    },
                    Err(e) => Err(e),
                })
                .collect::<Vec<std::result::Result<Option<_>, LedgerError>>>();
            if let Some(op_start_instant) = is_perf_enabled {
                // use multi-get instead
                report_rocksdb_read_perf(
                    C::NAME,
                    PERF_METRIC_OP_NAME_MULTI_GET,
                    &op_start_instant.elapsed(),
                    &self.column_options,
                );
            }

            result
        }
    }

    pub fn get(
        &self,
        key: C::Index,
    ) -> std::result::Result<Option<C::Type>, LedgerError> {
        self.get_raw(&C::key(key))
    }

    pub fn get_raw(
        &self,
        key: &[u8],
    ) -> std::result::Result<Option<C::Type>, LedgerError> {
        let mut result = Ok(None);
        let is_perf_enabled = maybe_enable_rocksdb_perf(
            self.column_options.rocks_perf_sample_interval,
            &self.read_perf_status,
        );
        if let Some(pinnable_slice) =
            self.backend.get_pinned_cf(self.handle(), key)?
        {
            let value = deserialize(pinnable_slice.as_ref())?;
            result = Ok(Some(value))
        }

        if let Some(op_start_instant) = is_perf_enabled {
            report_rocksdb_read_perf(
                C::NAME,
                PERF_METRIC_OP_NAME_GET,
                &op_start_instant.elapsed(),
                &self.column_options,
            );
        }
        result
    }

    pub fn put(
        &self,
        key: C::Index,
        value: &C::Type,
    ) -> std::result::Result<(), LedgerError> {
        let is_perf_enabled = maybe_enable_rocksdb_perf(
            self.column_options.rocks_perf_sample_interval,
            &self.write_perf_status,
        );
        let serialized_value = serialize(value)?;

        let result =
            self.backend
                .put_cf(self.handle(), &C::key(key), &serialized_value);

        if let Some(op_start_instant) = is_perf_enabled {
            report_rocksdb_write_perf(
                C::NAME,
                PERF_METRIC_OP_NAME_PUT,
                &op_start_instant.elapsed(),
                &self.column_options,
            );
        }
        result
    }
}

impl<C> LedgerColumn<C>
where
    C: ProtobufColumn + ColumnName,
{
    pub fn get_protobuf_or_bincode<T: DeserializeOwned + Into<C::Type>>(
        &self,
        key: C::Index,
    ) -> std::result::Result<Option<C::Type>, LedgerError> {
        self.get_raw_protobuf_or_bincode::<T>(&C::key(key))
    }

    pub(crate) fn get_raw_protobuf_or_bincode<
        T: DeserializeOwned + Into<C::Type>,
    >(
        &self,
        key: &[u8],
    ) -> std::result::Result<Option<C::Type>, LedgerError> {
        let is_perf_enabled = maybe_enable_rocksdb_perf(
            self.column_options.rocks_perf_sample_interval,
            &self.read_perf_status,
        );
        let result = self.backend.get_pinned_cf(self.handle(), key);
        if let Some(op_start_instant) = is_perf_enabled {
            report_rocksdb_read_perf(
                C::NAME,
                PERF_METRIC_OP_NAME_GET,
                &op_start_instant.elapsed(),
                &self.column_options,
            );
        }

        if let Some(pinnable_slice) = result? {
            let value = match C::Type::decode(pinnable_slice.as_ref()) {
                Ok(value) => value,
                Err(_) => deserialize::<T>(pinnable_slice.as_ref())?.into(),
            };
            Ok(Some(value))
        } else {
            Ok(None)
        }
    }

    pub fn get_protobuf(
        &self,
        key: C::Index,
    ) -> Result<Option<C::Type>, LedgerError> {
        let is_perf_enabled = maybe_enable_rocksdb_perf(
            self.column_options.rocks_perf_sample_interval,
            &self.read_perf_status,
        );
        let result = self.backend.get_pinned_cf(self.handle(), &C::key(key));
        if let Some(op_start_instant) = is_perf_enabled {
            report_rocksdb_read_perf(
                C::NAME,
                PERF_METRIC_OP_NAME_GET,
                &op_start_instant.elapsed(),
                &self.column_options,
            );
        }

        if let Some(pinnable_slice) = result? {
            Ok(Some(C::Type::decode(pinnable_slice.as_ref())?))
        } else {
            Ok(None)
        }
    }

    pub fn put_protobuf(
        &self,
        key: C::Index,
        value: &C::Type,
    ) -> std::result::Result<(), LedgerError> {
        let mut buf = Vec::with_capacity(value.encoded_len());
        value.encode(&mut buf)?;

        let is_perf_enabled = maybe_enable_rocksdb_perf(
            self.column_options.rocks_perf_sample_interval,
            &self.write_perf_status,
        );
        let result = self.backend.put_cf(self.handle(), &C::key(key), &buf);
        if let Some(op_start_instant) = is_perf_enabled {
            report_rocksdb_write_perf(
                C::NAME,
                PERF_METRIC_OP_NAME_PUT,
                &op_start_instant.elapsed(),
                &self.column_options,
            );
        }

        result
    }

    pub fn iter_protobuf(
        &self,
        iterator_mode: IteratorMode<C::Index>,
    ) -> impl Iterator<Item = LedgerResult<(C::Index, C::Type)>> + '_ {
        let cf = self.handle();
        let iter = self.backend.iterator_cf::<C>(cf, iterator_mode);
        iter.map(|pair| {
            let (key, value) = pair?;
            let decoded = C::Type::decode(value.as_ref())?;
            Ok((C::index(&key), decoded))
        })
    }
}

impl<C> LedgerColumn<C>
where
    C: ColumnIndexDeprecation + ColumnName,
{
    pub(crate) fn iter_current_index_filtered(
        &self,
        iterator_mode: IteratorMode<C::Index>,
    ) -> impl Iterator<Item = (C::Index, Box<[u8]>)> + '_ {
        let cf = self.handle();
        let iter = self.backend.iterator_cf::<C>(cf, iterator_mode);
        iter.filter_map(|pair| {
            let (key, value) = pair.unwrap();
            C::try_current_index(&key).ok().map(|index| (index, value))
        })
    }
}

/// When column key format is sequentially incremented key, like: `SlotColumn`
/// We can simplify extraction of count by calculating `LastKey - FirstKey + 1`
fn get_column_count_sequential_column<C: Column + ColumnName>(
    ledger_column: &LedgerColumn<C>,
) -> LedgerResult<u64> {
    let last_key = ledger_column.iter(IteratorMode::End)?.next();
    let start_key = ledger_column.iter(IteratorMode::Start)?.next();
    let count = match (start_key, last_key) {
        (Some((start_key, _)), Some((last_key, _))) => {
            let last_slot = C::slot(last_key);
            let start_slot = C::slot(start_key);

            last_slot - start_slot + 1
        }
        // Empty ColumnFamily
        _ => 0,
    };

    Ok(count)
}

/// For complex columns, like: `AddressSignatures`
/// We get column count using rocksdb's "estimate-num-keys" proprty
/// Due to properies of how we use DB this value shall be ~correct on start
fn get_column_count_complex_column<C: Column + ColumnName>(
    ledger_column: &LedgerColumn<C>,
) -> LedgerResult<u64> {
    const ESTIMATE_NUM_KEYS: &str = "rocksdb.estimate-num-keys";

    Ok(ledger_column.get_int_property(ESTIMATE_NUM_KEYS)? as u64)
}

/// Increases entries counter if it's not [`DIRTY_COUNT`]
/// Otherwise just skips it until it is set
pub fn try_increase_entry_counter(entry_counter: &AtomicI64, by: u64) {
    loop {
        let prev = entry_counter.load(Ordering::Acquire);
        if prev == DIRTY_COUNT {
            return;
        }

        // In case value changed to [`DIRTY_COUNT`] in between
        if entry_counter
            .compare_exchange(
                prev,
                prev + by as i64,
                Ordering::AcqRel,
                Ordering::Relaxed,
            )
            .is_ok()
        {
            return;
        }
    }
}

/// Decreases entries counter if it's not [`DIRTY_COUNT`]
/// Otherwise just skips it until it is set
pub fn try_decrease_entry_counter(entry_counter: &AtomicI64, by: u64) {
    loop {
        let prev = entry_counter.load(Ordering::Acquire);
        if prev == DIRTY_COUNT {
            return;
        }

        let new = prev - by as i64;
        if new >= 0 {
            // In case value changed to [`DIRTY_COUNT`] in between
            if entry_counter
                .compare_exchange(
                    prev,
                    new,
                    Ordering::AcqRel,
                    Ordering::Relaxed,
                )
                .is_ok()
            {
                return;
            }
        } else {
            warn!("Negative entry counter!");
            // In case value fixed to valid one in between
            if entry_counter
                .compare_exchange(
                    prev,
                    DIRTY_COUNT,
                    Ordering::AcqRel,
                    Ordering::Relaxed,
                )
                .is_ok()
            {
                return;
            }
        }
    }
}
