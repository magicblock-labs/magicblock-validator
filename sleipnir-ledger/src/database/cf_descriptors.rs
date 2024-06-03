use std::{collections::HashSet, path::Path};

use log::*;
use rocksdb::{ColumnFamilyDescriptor, DBCompressionType, Options, DB};

use super::{
    columns::{should_enable_compression, Column, ColumnName},
    consts,
    options::{LedgerColumnOptions, LedgerOptions},
    rocksdb_options::should_disable_auto_compactions,
};
use crate::database::{columns, options::AccessType};

/// Create the column family (CF) descriptors necessary to open the database.
///
/// In order to open a RocksDB database with Primary access, all columns must be opened. So,
/// in addition to creating descriptors for all of the expected columns, also create
/// descriptors for columns that were discovered but are otherwise unknown to the software.
///
/// One case where columns could be unknown is if a RocksDB database is modified with a newer
/// software version that adds a new column, and then also opened with an older version that
/// did not have knowledge of that new column.
pub fn cf_descriptors(
    path: &Path,
    options: &LedgerOptions,
) -> Vec<ColumnFamilyDescriptor> {
    use columns::*;

    let mut cf_descriptors = vec![
        new_cf_descriptor::<TransactionStatus>(options),
        new_cf_descriptor::<AddressSignatures>(options),
        new_cf_descriptor::<SlotSignatures>(options),
        new_cf_descriptor::<TransactionStatusIndex>(options),
        new_cf_descriptor::<Blocktime>(options),
        new_cf_descriptor::<Transaction>(options),
        new_cf_descriptor::<TransactionMemos>(options),
        new_cf_descriptor::<PerfSamples>(options),
    ];

    // If the access type is Secondary, we don't need to open all of the
    // columns so we can just return immediately.
    match options.access_type {
        AccessType::Secondary => {
            return cf_descriptors;
        }
        AccessType::Primary | AccessType::PrimaryForMaintenance => {}
    }

    // Attempt to detect the column families that are present. It is not a
    // fatal error if we cannot, for example, if the Blockstore is brand
    // new and will be created by the call to Rocks::open().
    let detected_cfs = match DB::list_cf(&Options::default(), path) {
        Ok(detected_cfs) => detected_cfs,
        Err(err) => {
            warn!("Unable to detect Rocks columns: {err:?}");
            vec![]
        }
    };

    // The default column is handled automatically, we don't need to create
    // a descriptor for it
    const DEFAULT_COLUMN_NAME: &str = "default";
    let known_cfs: HashSet<_> = cf_descriptors
        .iter()
        .map(|cf_descriptor| cf_descriptor.name().to_string())
        .chain(std::iter::once(DEFAULT_COLUMN_NAME.to_string()))
        .collect();
    detected_cfs.iter().for_each(|cf_name| {
            if !known_cfs.contains(cf_name.as_str()) {
                info!("Detected unknown column {cf_name}, opening column with basic options");
                // This version of the software was unaware of the column, so
                // it is fair to assume that we will not attempt to read or
                // write the column. So, set some bare bones settings to avoid
                // using extra resources on this unknown column.
                let mut options = Options::default();
                // Lower the default to avoid unnecessary allocations
                options.set_write_buffer_size(1024 * 1024);
                // Disable compactions to avoid any modifications to the column
                options.set_disable_auto_compactions(true);
                cf_descriptors.push(ColumnFamilyDescriptor::new(cf_name, options));
            }
        });

    cf_descriptors
}

fn new_cf_descriptor<C: 'static + Column + ColumnName>(
    options: &LedgerOptions,
) -> ColumnFamilyDescriptor {
    ColumnFamilyDescriptor::new(C::NAME, get_cf_options::<C>(options))
}

// FROM ledger/src/blockstore_db.rs :2010
fn get_cf_options<C: 'static + Column + ColumnName>(
    options: &LedgerOptions,
) -> Options {
    let mut cf_options = Options::default();
    // 256 * 8 = 2GB. 6 of these columns should take at most 12GB of RAM
    cf_options.set_max_write_buffer_number(8);
    cf_options.set_write_buffer_size(consts::MAX_WRITE_BUFFER_SIZE as usize);
    let file_num_compaction_trigger = 4;
    // Recommend that this be around the size of level 0. Level 0 estimated size in stable state is
    // write_buffer_size * min_write_buffer_number_to_merge * level0_file_num_compaction_trigger
    // Source: https://docs.rs/rocksdb/0.6.0/rocksdb/struct.Options.html#method.set_level_zero_file_num_compaction_trigger
    let total_size_base =
        consts::MAX_WRITE_BUFFER_SIZE * file_num_compaction_trigger;
    let file_size_base = total_size_base / 10;
    cf_options.set_level_zero_file_num_compaction_trigger(
        file_num_compaction_trigger as i32,
    );
    cf_options.set_max_bytes_for_level_base(total_size_base);
    cf_options.set_target_file_size_base(file_size_base);

    let disable_auto_compactions =
        should_disable_auto_compactions(&options.access_type);
    if disable_auto_compactions {
        cf_options.set_disable_auto_compactions(true);
    }

    process_cf_options_advanced::<C>(&mut cf_options, &options.column_options);

    cf_options
}

fn process_cf_options_advanced<C: 'static + Column + ColumnName>(
    cf_options: &mut Options,
    column_options: &LedgerColumnOptions,
) {
    // Explicitly disable compression on all columns by default
    // See https://docs.rs/rocksdb/0.21.0/rocksdb/struct.Options.html#method.set_compression_type
    cf_options.set_compression_type(DBCompressionType::None);

    if should_enable_compression::<C>() {
        cf_options.set_compression_type(
            column_options
                .compression_type
                .to_rocksdb_compression_type(),
        );
    }
}
