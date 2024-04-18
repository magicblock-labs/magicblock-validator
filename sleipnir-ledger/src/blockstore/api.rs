use bincode::deserialize;
use log::*;
use solana_measure::measure::Measure;
use solana_sdk::clock::Slot;
use std::fs;
use std::path::Path;
use std::sync::RwLock;
use std::{path::PathBuf, sync::Arc};

use crate::database::columns as cf;
use crate::database::db::Database;
use crate::database::errors::BlockstoreResult;
use crate::database::iterator::IteratorMode;
use crate::database::ledger_column::LedgerColumn;
use crate::database::meta::TransactionStatusIndexMeta;
use crate::database::options::BlockstoreOptions;

pub struct Blockstore {
    ledger_path: PathBuf,
    db: Arc<Database>,

    transaction_status_index_cf: LedgerColumn<cf::TransactionStatusIndex>,

    highest_primary_index_slot: RwLock<Option<Slot>>,
}

impl Blockstore {
    pub fn db(self) -> Arc<Database> {
        self.db
    }

    pub fn ledger_path(&self) -> &PathBuf {
        &self.ledger_path
    }

    pub fn banking_trace_path(&self) -> PathBuf {
        self.ledger_path.join("banking_trace")
    }

    /// Opens a Ledger in directory, provides "infinite" window of shreds
    pub fn open(ledger_path: &Path) -> BlockstoreResult<Self> {
        Self::do_open(ledger_path, BlockstoreOptions::default())
    }

    pub fn open_with_options(
        ledger_path: &Path,
        options: BlockstoreOptions,
    ) -> BlockstoreResult<Self> {
        Self::do_open(ledger_path, options)
    }

    fn do_open(
        ledger_path: &Path,
        options: BlockstoreOptions,
    ) -> BlockstoreResult<Self> {
        fs::create_dir_all(ledger_path)?;
        let blockstore_path = ledger_path.join(
            options
                .column_options
                .shred_storage_type
                .blockstore_directory(),
        );
        // TODO:
        // adjust_ulimit_nofile(options.enforce_ulimit_nofile)?;

        // Open the database
        let mut measure = Measure::start("blockstore open");
        info!("Opening blockstore at {:?}", blockstore_path);
        let db = Database::open(&blockstore_path, options)?;

        let transaction_status_index_cf = db.column();

        let db = Arc::new(db);

        // NOTE: left out max root

        measure.stop();
        info!("Opening blockstore done; {measure}");

        let blockstore = Blockstore {
            ledger_path: ledger_path.to_path_buf(),
            db,
            transaction_status_index_cf,

            highest_primary_index_slot: RwLock::<Option<Slot>>::default(),
        };

        blockstore.cleanup_old_entries()?;
        blockstore.update_highest_primary_index_slot()?;

        Ok(blockstore)
    }

    /// Collects and reports [`BlockstoreRocksDbColumnFamilyMetrics`] for the
    /// all the column families.
    ///
    /// [`BlockstoreRocksDbColumnFamilyMetrics`]: crate::blockstore_metrics::BlockstoreRocksDbColumnFamilyMetrics
    pub fn submit_rocksdb_cf_metrics_for_all_cfs(&self) {
        self.transaction_status_index_cf.submit_rocksdb_cf_metrics();
    }

    fn cleanup_old_entries(&self) -> BlockstoreResult<()> {
        if !self.is_primary_access() {
            return Ok(());
        }

        // Initialize TransactionStatusIndexMeta if they are not present already
        if self.transaction_status_index_cf.get(0)?.is_none() {
            self.transaction_status_index_cf
                .put(0, &TransactionStatusIndexMeta::default())?;
        }
        if self.transaction_status_index_cf.get(1)?.is_none() {
            self.transaction_status_index_cf
                .put(1, &TransactionStatusIndexMeta::default())?;
        }
        Ok(())
    }

    fn set_highest_primary_index_slot(&self, slot: Option<Slot>) {
        *self.highest_primary_index_slot.write().unwrap() = slot;
    }

    fn update_highest_primary_index_slot(&self) -> BlockstoreResult<()> {
        let iterator =
            self.transaction_status_index_cf.iter(IteratorMode::Start)?;
        let mut highest_primary_index_slot = None;
        for (_, data) in iterator {
            let meta: TransactionStatusIndexMeta = deserialize(&data).unwrap();
            if highest_primary_index_slot.is_none()
                || highest_primary_index_slot
                    .is_some_and(|slot| slot < meta.max_slot)
            {
                highest_primary_index_slot = Some(meta.max_slot);
            }
        }
        if highest_primary_index_slot.is_some_and(|slot| slot != 0) {
            self.set_highest_primary_index_slot(highest_primary_index_slot);
        }
        Ok(())
    }

    /// Returns whether the blockstore has primary (read and write) access
    pub fn is_primary_access(&self) -> bool {
        self.db.is_primary_access()
    }
}
