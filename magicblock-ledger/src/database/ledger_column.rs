use std::{marker::PhantomData, sync::Arc};

use bincode::{deserialize, serialize};
use prost::Message;
use rocksdb::{properties as RocksProperties, ColumnFamily};
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
    ) -> std::result::Result<Option<Vec<u8>>, LedgerError> {
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
        name: &'static std::ffi::CStr,
    ) -> std::result::Result<i64, LedgerError> {
        self.backend.get_int_property_cf(self.handle(), name)
    }

    pub fn delete(
        &self,
        key: C::Index,
    ) -> std::result::Result<(), LedgerError> {
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
    ) -> std::result::Result<Option<C::Type>, LedgerError> {
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
    ) -> LedgerResult<impl Iterator<Item = (C::Index, Box<[u8]>)> + '_> {
        let cf = self.handle();
        let iter = self.backend.iterator_cf::<C>(cf, iterator_mode);
        Ok(iter.filter_map(|pair| {
            let (key, value) = pair.unwrap();
            C::try_current_index(&key).ok().map(|index| (index, value))
        }))
    }
}
