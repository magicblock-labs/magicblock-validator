use std::{
    fmt, fs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicI64, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

use bincode::{deserialize, serialize};
use magicblock_core::link::blocks::{BlockHash, LatestBlockInner};
use magicblock_metrics::metrics::{
    start_ledger_disable_compactions_timer, start_ledger_shutdown_timer,
    HistogramTimer,
};
use prost::Message;
use rocksdb::{AsRawPtr, Direction as IteratorDirection, FlushOptions};
use solana_clock::{Slot, UnixTimestamp};
use solana_hash::{Hash, HASH_BYTES};
use solana_measure::measure::Measure;
use solana_pubkey::Pubkey;
use solana_signature::{Signature, SIGNATURE_BYTES};
use solana_storage_proto::convert::generated;
use solana_transaction::versioned::VersionedTransaction;
use solana_transaction_status::{
    ConfirmedTransactionStatusWithSignature,
    ConfirmedTransactionWithStatusMeta, TransactionStatusMeta,
    TransactionWithStatusMeta, VersionedConfirmedBlock,
    VersionedTransactionWithStatusMeta,
};
use tracing::*;

use crate::{
    database::{
        columns::{self as cf, Column, ColumnName, DIRTY_COUNT},
        db::Database,
        iterator::IteratorMode,
        ledger_column::{try_increase_entry_counter, LedgerColumn},
        meta::{AddressSignatureMeta, PerfSample},
        options::LedgerOptions,
    },
    errors::{LedgerError, LedgerResult},
    metrics::LedgerRpcApiMetrics,
    store::utils::adjust_ulimit_nofile,
    LatestBlock,
};

#[derive(Default, Debug)]
pub struct SignatureInfosForAddress {
    pub infos: Vec<ConfirmedTransactionStatusWithSignature>,
    pub found_upper: bool,
    pub found_lower: bool,
}

pub struct Ledger {
    ledger_path: PathBuf,
    db: Arc<Database>,

    blocktime_cf: LedgerColumn<cf::Blocktime>,
    blockhash_cf: LedgerColumn<cf::Blockhash>,
    slot_signatures_cf: LedgerColumn<cf::SlotSignatures>,
    address_signatures_cf: LedgerColumn<cf::AddressSignatures>,
    transaction_status_cf: LedgerColumn<cf::TransactionStatus>,
    transaction_cf: LedgerColumn<cf::Transaction>,
    transaction_memos_cf: LedgerColumn<cf::TransactionMemos>,
    perf_samples_cf: LedgerColumn<cf::PerfSamples>,

    transaction_successful_status_count: AtomicI64,
    transaction_failed_status_count: AtomicI64,

    rpc_api_metrics: LedgerRpcApiMetrics,
    latest_block: LatestBlock,
}

impl fmt::Display for Ledger {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Ledger at {:?}", self.ledger_path)
    }
}

impl Ledger {
    pub fn db(self) -> Arc<Database> {
        self.db
    }

    pub fn ledger_path(&self) -> &PathBuf {
        &self.ledger_path
    }

    pub fn banking_trace_path(&self) -> PathBuf {
        self.ledger_path.join("banking_trace")
    }

    pub fn storage_size(&self) -> Result<u64, LedgerError> {
        self.db.storage_size()
    }

    /// Opens a Ledger in directory, provides "infinite" window of shreds
    pub fn open(ledger_path: &Path) -> Result<Self, LedgerError> {
        Self::do_open(ledger_path, LedgerOptions::default())
    }

    pub fn open_with_options(
        ledger_path: &Path,
        options: LedgerOptions,
    ) -> Result<Self, LedgerError> {
        Self::do_open(ledger_path, options)
    }

    fn do_open(
        ledger_path: &Path,
        options: LedgerOptions,
    ) -> Result<Self, LedgerError> {
        fs::create_dir_all(ledger_path)?;
        let ledger_path = ledger_path.join(
            options
                .column_options
                .shred_storage_type
                .blockstore_directory(),
        );
        adjust_ulimit_nofile(options.enforce_ulimit_nofile)?;

        // Open the database
        let mut measure = Measure::start("ledger open");
        info!(path = ?ledger_path, "Opening ledger");
        let db = Database::open(&ledger_path, options)?;

        let transaction_status_cf = db.column();
        let address_signatures_cf = db.column();
        let slot_signatures_cf = db.column();
        let blocktime_cf = db.column();
        let blockhash_cf = db.column();
        let transaction_cf = db.column();
        let transaction_memos_cf = db.column();
        let perf_samples_cf = db.column();

        let db = Arc::new(db);

        // NOTE: left out max root

        measure.stop();
        info!("Opening ledger done; {measure}");
        let latest_block = LatestBlock::default();

        let ledger = Ledger {
            ledger_path: ledger_path.to_path_buf(),
            db,

            transaction_status_cf,
            address_signatures_cf,
            slot_signatures_cf,
            blocktime_cf,
            blockhash_cf,
            transaction_cf,
            transaction_memos_cf,
            perf_samples_cf,

            transaction_successful_status_count: AtomicI64::new(DIRTY_COUNT),
            transaction_failed_status_count: AtomicI64::new(DIRTY_COUNT),

            rpc_api_metrics: LedgerRpcApiMetrics::default(),
            latest_block,
        };
        let (slot, blockhash) = ledger.get_max_blockhash()?;
        let time = ledger.get_block_time(slot)?.unwrap_or_default();
        let block = LatestBlockInner::new(slot, blockhash, time);
        ledger.latest_block.store(block);
        let oldest_slot = ledger.get_lowest_slot()?.unwrap_or_default();
        info!(oldest_slot, "Initializing ledger retention boundary");
        ledger.set_oldest_slot(oldest_slot);

        Ok(ledger)
    }

    /// Collects and reports [`BlockstoreRocksDbColumnFamilyMetrics`] for
    /// all the column families.
    ///
    /// [`BlockstoreRocksDbColumnFamilyMetrics`]: crate::blockstore_metrics::BlockstoreRocksDbColumnFamilyMetrics
    pub fn submit_rocksdb_cf_metrics_for_all_cfs(&self) {
        self.transaction_status_cf.submit_rocksdb_cf_metrics();
        self.address_signatures_cf.submit_rocksdb_cf_metrics();
        self.slot_signatures_cf.submit_rocksdb_cf_metrics();
        self.blocktime_cf.submit_rocksdb_cf_metrics();
        self.blockhash_cf.submit_rocksdb_cf_metrics();
        self.transaction_cf.submit_rocksdb_cf_metrics();
        self.transaction_memos_cf.submit_rocksdb_cf_metrics();
        self.perf_samples_cf.submit_rocksdb_cf_metrics();
    }

    // -----------------
    // Cleanup boundary
    // -----------------

    /// One retention snapshot and one final validation per logical read.
    /// Register the lowest slot used before accessing its data, including cursor
    /// dependencies, so cleanup also takes precedence on errors and early returns.
    fn read_range<T>(
        &self,
        read: impl FnOnce(Slot, &mut Slot) -> LedgerResult<T>,
    ) -> LedgerResult<T> {
        let oldest = self.oldest_slot();
        let mut lowest = Slot::MAX;
        let result = read(oldest, &mut lowest);
        if lowest < self.oldest_slot() {
            return Err(LedgerError::SlotCleanedUp);
        }
        result
    }

    /// Checks the entire operation, including error and early-return paths.
    /// Cleanup takes precedence over missing rows in a partially retired slot.
    fn read_slot<T>(
        &self,
        slot: Slot,
        read: impl FnOnce(Slot) -> LedgerResult<T>,
    ) -> LedgerResult<T> {
        self.read_range(|oldest, lowest| {
            *lowest = slot;
            if slot < oldest {
                return Err(LedgerError::SlotCleanedUp);
            }
            read(oldest)
        })
    }

    /// Raw point reads report retired data as absent; assembled history reads
    /// use `read_slot` directly so a partial result cannot appear complete.
    fn read_optional<T>(
        &self,
        slot: Slot,
        read: impl FnOnce() -> LedgerResult<Option<T>>,
    ) -> LedgerResult<Option<T>> {
        match self.read_slot(slot, |_| read()) {
            Err(LedgerError::SlotCleanedUp) => Ok(None),
            result => result,
        }
    }

    /// Returns lowest slot in the ledger if there's any
    pub fn get_lowest_slot(&self) -> Result<Option<Slot>, LedgerError> {
        Ok(self
            .blockhash_cf
            .iter(IteratorMode::Start)?
            .next()
            .map(|(slot, _)| slot))
    }

    /// The inclusive lower bound shared by reads and compaction. Zero means
    /// no slots have been retired, including slot zero.
    pub fn oldest_slot(&self) -> Slot {
        self.db.oldest_slot()
    }

    // -----------------
    // Block time
    // -----------------

    pub fn get_block_time(
        &self,
        slot: Slot,
    ) -> LedgerResult<Option<UnixTimestamp>> {
        self.read_slot(slot, |_| self.blocktime_cf.get(slot))
    }

    pub fn count_block_times(&self) -> LedgerResult<i64> {
        self.blocktime_cf.count_column_using_cache()
    }

    // -----------------
    // Blockhash
    // -----------------

    pub fn count_blockhashes(&self) -> LedgerResult<i64> {
        self.blockhash_cf.count_column_using_cache()
    }

    pub fn get_max_blockhash(&self) -> LedgerResult<(Slot, Hash)> {
        let mut iter = self.blockhash_cf.iter(IteratorMode::End)?;
        let (slot, hash_vec) =
            iter.next().unwrap_or((0, Box::new([0; HASH_BYTES])));
        let hash = <[u8; HASH_BYTES]>::try_from(hash_vec.as_ref())
            .map(Hash::new_from_array)
            .expect("failed to construct hash from slice");
        Ok((slot, hash))
    }

    /// Returns the highest transaction index for a given slot.
    ///
    /// Uses a reverse iterator from `(slot, u32::MAX)` to find the first
    /// (highest) index in O(1) time.
    ///
    /// Returns `None` if no transactions exist in the slot.
    pub fn get_highest_transaction_index_for_slot(
        &self,
        slot: Slot,
    ) -> LedgerResult<Option<u32>> {
        let mut iter = self.slot_signatures_cf.iter(IteratorMode::From(
            (slot, u32::MAX),
            IteratorDirection::Reverse,
        ))?;

        match iter.next() {
            Some(((tx_slot, tx_index), _)) if tx_slot == slot => {
                Ok(Some(tx_index))
            }
            _ => Ok(None),
        }
    }

    /// Returns the position (slot, index) of the most recent transaction.
    ///
    /// This is useful for resuming replication from the last known position.
    /// Returns `None` if no transactions exist in the ledger.
    pub fn get_latest_transaction_position(
        &self,
    ) -> LedgerResult<Option<(Slot, u32)>> {
        let (latest_slot, _) = self.get_max_blockhash()?;

        // Try to find the highest index in the latest slot
        if let Some(index) =
            self.get_highest_transaction_index_for_slot(latest_slot)?
        {
            return Ok(Some((latest_slot, index)));
        }

        // If the latest slot has no transactions, check previous slots
        // by iterating backwards through slot_signatures_cf
        let mut iter = self.slot_signatures_cf.iter(IteratorMode::End)?;

        if let Some(((slot, index), _)) = iter.next() {
            Ok(Some((slot, index)))
        } else {
            Ok(None)
        }
    }

    /// Returns the position `(slot, index)` of the highest key persisted in
    /// `slot_signatures_cf`, regardless of whether that slot has a finalized
    /// block header.
    ///
    /// Unlike [`Self::get_latest_transaction_position`], which prefers the
    /// latest *finalized* (blockhash) slot, this returns the true maximum key.
    /// This is what a restarting replica needs to resume deduplication from the
    /// last transaction of an in-progress (not-yet-finalized) slot.
    ///
    /// Returns `None` if no transactions exist in the ledger.
    pub fn get_last_persisted_transaction_position(
        &self,
    ) -> LedgerResult<Option<(Slot, u32)>> {
        let mut iter = self.slot_signatures_cf.iter(IteratorMode::End)?;
        Ok(iter.next().map(|((slot, index), _)| (slot, index)))
    }

    /// Returns the signatures of all transactions persisted for `slot`, in
    /// ascending transaction-index order (the same order in which the primary
    /// scheduled and hashed them).
    ///
    /// Works for in-progress slots that have no finalized block header yet,
    /// unlike [`Self::get_block`]. Used to rebuild the streaming-blockhash
    /// accumulator after a mid-slot replica restart.
    pub fn get_transaction_signatures_for_slot(
        &self,
        slot: Slot,
    ) -> LedgerResult<Vec<Signature>> {
        let iter = self
            .slot_signatures_cf
            .iter(IteratorMode::From((slot, 0), IteratorDirection::Forward))?;

        let mut signatures = Vec::new();
        for ((tx_slot, _tx_index), tx_signature) in iter {
            if tx_slot != slot {
                break;
            }
            signatures.push(Signature::try_from(&*tx_signature)?);
        }
        Ok(signatures)
    }

    // -----------------
    // Block
    // -----------------

    // NOTE: we kept the term block time even tough we don't produce blocks.
    // As far as we are concerned these are just the time when we advanced to
    // a specific slot.
    pub fn write_block(&self, block: LatestBlockInner) -> LedgerResult<()> {
        // Blocktime and blockhash must land atomically: readers treat the
        // pair as one record (e.g. getBlockTime vs getBlock availability),
        // so a split write must never be observable or partially persisted.
        let mut batch = self.db.batch();
        batch.put::<cf::Blocktime>(block.slot, &block.clock.unix_timestamp)?;
        batch.put::<cf::Blockhash>(block.slot, &block.blockhash)?;
        self.db.write(batch)?;

        self.blocktime_cf.try_increase_entry_counter(1);
        self.blockhash_cf.try_increase_entry_counter(1);
        self.latest_block.store(block);
        Ok(())
    }

    /// Returns a retained block. A missing or retired parent has the default
    /// previous blockhash; cleanup of the requested slot rejects the whole read.
    pub fn get_block(
        &self,
        slot: Slot,
    ) -> LedgerResult<Option<VersionedConfirmedBlock>> {
        self.read_slot(slot, |oldest| {
            let Some(blockhash) = self.blockhash_cf.get(slot)? else {
                return Ok(None);
            };
            let Some(block_time) = self.blocktime_cf.get(slot)? else {
                return Ok(None);
            };
            let parent_slot = slot.saturating_sub(1);
            // Parent metadata is optional; only the requested block must remain
            // retained for the entire operation.
            let previous_blockhash = if parent_slot >= oldest {
                self.blockhash_cf.get(parent_slot)
            } else {
                Ok(None)
            }?;

            // Decode directly from the index iterator instead of first allocating
            // a second vector of signatures. The outer read checks the whole block.
            let transactions = self
                .slot_signatures_cf
                .iter_current_index_filtered(IteratorMode::From(
                    (slot, u32::MAX),
                    IteratorDirection::Reverse,
                ))
                .take_while(|((tx_slot, _), _)| *tx_slot == slot)
                .map(|(_, bytes)| {
                    let signature = Signature::try_from(&*bytes)?;
                    let transaction = self
                        .transaction((signature, slot))?
                        .ok_or(LedgerError::TransactionNotFound)?;
                    let meta = self
                        .transaction_status((signature, slot))?
                        .ok_or(LedgerError::TransactionStatusMetaNotFound)?;
                    Ok(VersionedTransactionWithStatusMeta { transaction, meta })
                })
                .collect::<LedgerResult<Vec<_>>>()?;

            Ok(Some(VersionedConfirmedBlock {
                previous_blockhash: previous_blockhash
                    .unwrap_or_default()
                    .to_string(),
                blockhash: blockhash.to_string(),
                parent_slot,
                transactions,
                rewards: vec![], // This validator doesn't do voting.
                block_time: Some(block_time),
                block_height: Some(slot),
                num_partitions: None,
            }))
        })
    }

    pub fn count_slot_signatures(&self) -> LedgerResult<i64> {
        self.slot_signatures_cf.count_column_using_cache()
    }

    // -----------------
    // Signatures
    // -----------------

    /// Returns address history in descending `(slot, transaction_index)` order.
    /// `before` and `until` are exclusive cursors, even within the same slot or
    /// when their transactions do not mention this address. Unknown cursors are
    /// ignored. `highest_slot` and the cleanup boundary always bound the scan.
    pub fn get_confirmed_signatures_for_address(
        &self,
        pubkey: Pubkey,
        highest_slot: Slot,
        before: Option<Signature>,
        until: Option<Signature>,
        limit: usize,
    ) -> LedgerResult<SignatureInfosForAddress> {
        self.rpc_api_metrics
            .num_get_confirmed_signatures_for_address
            .fetch_add(1, Ordering::Relaxed);

        self.read_range(|oldest_slot, lowest| {
            let before = before
                .map(|signature| {
                    self.signature_position(signature, oldest_slot, lowest)
                })
                .transpose()?
                .flatten();
            let until = until
                .map(|signature| {
                    self.signature_position(signature, oldest_slot, lowest)
                })
                .transpose()?
                .flatten();
            let mut result = SignatureInfosForAddress {
                found_upper: before.is_some(),
                found_lower: until.is_some(),
                ..Default::default()
            };
            let highest = (highest_slot, u32::MAX);
            let start = before.unwrap_or(highest).min(highest);
            // Preserve the history API's exclusion of genesis-slot transactions.
            let oldest_slot = oldest_slot.max(1);
            if limit == 0
                || start.0 < oldest_slot
                || until.is_some_and(|end| start <= end)
            {
                return Ok(result);
            }

            let iterator = self
                .address_signatures_cf
                .iter_current_index_filtered(IteratorMode::From(
                    (
                        pubkey,
                        start.0,
                        start.1,
                        Signature::from([u8::MAX; SIGNATURE_BYTES]),
                    ),
                    IteratorDirection::Reverse,
                ));
            // Slots are contiguous in this ordering, so only the last blocktime is
            // needed. No matching-signature vector or per-request HashMap is required.
            let mut blocktime = None;
            for ((address, slot, index, signature), _) in iterator {
                let position = (slot, index);
                if address != pubkey
                    || slot < oldest_slot
                    || until.is_some_and(|end| position <= end)
                {
                    break;
                }
                if before.is_some_and(|start| position >= start) {
                    continue;
                }
                *lowest = (*lowest).min(slot);
                if blocktime.is_none_or(|(cached_slot, _)| cached_slot != slot)
                {
                    blocktime = Some((slot, self.blocktime_cf.get(slot)?));
                }
                let status = self.transaction_status((signature, slot))?;
                result.infos.push(ConfirmedTransactionStatusWithSignature {
                    slot,
                    signature,
                    block_time: blocktime.and_then(|(_, time)| time),
                    err: status.and_then(|meta| meta.status.err()),
                    memo: self.transaction_memos_cf.get((signature, slot))?,
                    index: 0,
                });
                if result.infos.len() == limit {
                    break;
                }
            }
            Ok(result)
        })
    }

    /// Resolve cursors through the slot index, not the queried address: a valid
    /// pagination cursor need not have touched that address.
    fn signature_position(
        &self,
        signature: Signature,
        oldest_slot: Slot,
        lowest: &mut Slot,
    ) -> LedgerResult<Option<(Slot, u32)>> {
        let Some((slot, _)) =
            self.status_entry(signature, Slot::MAX, oldest_slot)
        else {
            return Ok(None);
        };
        *lowest = (*lowest).min(slot);
        self.slot_signatures_cf
            .iter_current_index_filtered(IteratorMode::From(
                (slot, u32::MAX),
                IteratorDirection::Reverse,
            ))
            .take_while(|((tx_slot, _), _)| *tx_slot == slot)
            .find_map(|((_, index), bytes)| {
                (bytes.as_ref() == signature.as_ref()).then_some((slot, index))
            })
            .map(Some)
            .ok_or(LedgerError::TransactionNotFound)
    }

    pub fn count_address_signatures(&self) -> LedgerResult<i64> {
        self.address_signatures_cf.count_column_using_cache()
    }

    // -----------------
    // Transaction
    // -----------------
    pub fn get_complete_transaction(
        &self,
        signature: Signature,
        highest_confirmed_slot: Slot,
    ) -> LedgerResult<Option<ConfirmedTransactionWithStatusMeta>> {
        self.rpc_api_metrics
            .num_get_complete_transaction
            .fetch_add(1, Ordering::Relaxed);
        self.read_range(|oldest, lowest| {
            let (slot, meta) = match self
                .status_entry(signature, highest_confirmed_slot, oldest)
            {
                Some((slot, bytes)) => {
                    *lowest = slot;
                    let meta = generated::TransactionStatusMeta::decode(bytes.as_ref())?;
                    (slot, Some(Self::convert_status(meta, slot)?))
                }
                None => {
                    let mut iterator = self
                        .transaction_cf
                        .iter_current_index_filtered(IteratorMode::From(
                            (signature, highest_confirmed_slot),
                            IteratorDirection::Forward,
                        ));
                    let Some(((found, slot), _)) = iterator.next() else {
                        return Ok(None);
                    };
                    if found != signature || slot > highest_confirmed_slot {
                        return Ok(None);
                    }
                    *lowest = slot;
                    if slot < oldest {
                        return Err(LedgerError::SlotCleanedUp);
                    }
                    (slot, None)
                }
            };

            let Some(transaction) = self.transaction((signature, slot))? else {
                return Ok(None);
            };
            let tx_with_meta = match meta {
                Some(meta) => TransactionWithStatusMeta::Complete(
                    VersionedTransactionWithStatusMeta { transaction, meta },
                ),
                None => TransactionWithStatusMeta::MissingMetadata(
                    transaction.into_legacy_transaction().ok_or_else(|| {
                        LedgerError::TransactionConversionError(
                            "failed to convert versioned transaction to legacy: \
                             transaction is v0 (requires metadata)".to_owned(),
                        )
                    })?,
                ),
            };
            Ok(Some(ConfirmedTransactionWithStatusMeta {
                slot,
                block_time: self.blocktime_cf.get(slot)?,
                tx_with_meta,
                index: 0,
            }))
        })
    }

    /// Writes a confirmed transaction pieced together from the provided inputs
    /// * `signature` - Signature of the transaction
    /// * `slot` - Slot at which the transaction was confirmed
    /// * `writable_keys` - Writable account keys from the transaction
    /// * `readonly_keys` - Readonly account keys from the transaction
    /// * `encoded_transaction` - Bincode-serialized `VersionedTransaction`
    /// * `status` - status of the transaction
    #[allow(clippy::too_many_arguments)]
    pub fn write_transaction(
        &self,
        signature: Signature,
        slot: Slot,
        index: u32,
        writable_keys: Vec<&Pubkey>,
        readonly_keys: Vec<&Pubkey>,
        encoded_transaction: &[u8],
        status: TransactionStatusMeta,
    ) -> LedgerResult<()> {
        // 1. Write Transaction Status
        self.write_transaction_status(
            slot,
            index,
            signature,
            writable_keys,
            readonly_keys,
            status,
        )?;

        // 2. Write Transaction (raw bincode bytes)
        self.transaction_cf
            .put_bytes((signature, slot), encoded_transaction)?;
        self.transaction_cf.try_increase_entry_counter(1);

        Ok(())
    }

    pub fn read_transaction(
        &self,
        index: (Signature, Slot),
    ) -> LedgerResult<Option<VersionedTransaction>> {
        self.read_optional(index.1, || self.transaction(index))
    }

    /// Raw decoding shared by standalone and compound reads. The caller owns
    /// retention validation for the complete operation.
    fn transaction(
        &self,
        index: (Signature, Slot),
    ) -> LedgerResult<Option<VersionedTransaction>> {
        self.transaction_cf
            .get_bytes(index)?
            .map(|bytes| deserialize(&bytes).map_err(Into::into))
            .transpose()
    }

    /// Verifies the signature of a transaction stored in the ledger.
    ///
    /// Returns:
    /// - `None` if no transaction with that signature exists
    /// - `Some(true)` if the transaction exists and its signature is valid
    /// - `Some(false)` if the transaction exists but its signature is
    ///   invalid
    pub fn verify_transaction_signature(
        &self,
        signature: &Signature,
    ) -> LedgerResult<Option<bool>> {
        self.read_range(|oldest, lowest| {
            let Some((slot, _)) =
                self.status_entry(*signature, Slot::MAX, oldest)
            else {
                return Ok(None);
            };
            *lowest = slot;
            Ok(self.transaction((*signature, slot))?.map(|transaction| {
                transaction.verify_and_hash_message().is_ok()
            }))
        })
    }

    pub fn count_transactions(&self) -> LedgerResult<i64> {
        self.transaction_cf.count_column_using_cache()
    }

    // -----------------
    // TransactionMemos
    // -----------------
    pub fn read_transaction_memos(
        &self,
        signature: Signature,
        slot: Slot,
    ) -> LedgerResult<Option<String>> {
        self.read_optional(slot, || {
            self.transaction_memos_cf.get((signature, slot))
        })
    }

    pub fn write_transaction_memos(
        &self,
        signature: &Signature,
        slot: Slot,
        memos: String,
    ) -> LedgerResult<()> {
        let res = self.transaction_memos_cf.put((*signature, slot), &memos);
        self.transaction_memos_cf.try_increase_entry_counter(1);
        res
    }

    pub fn count_transaction_memos(&self) -> LedgerResult<i64> {
        self.transaction_memos_cf.count_column_using_cache()
    }

    // -----------------
    // TransactionStatus
    // -----------------
    /// Returns the first retained status for `signature` at or below `highest_slot`.
    pub fn get_transaction_status(
        &self,
        signature: Signature,
        highest_slot: Slot,
    ) -> LedgerResult<Option<(Slot, TransactionStatusMeta)>> {
        self.read_range(|oldest, lowest| {
            let Some((slot, bytes)) =
                self.status_entry(signature, highest_slot, oldest)
            else {
                return Ok(None);
            };
            *lowest = slot;
            let meta =
                generated::TransactionStatusMeta::decode(bytes.as_ref())?;
            Ok(Some((slot, Self::convert_status(meta, slot)?)))
        })
    }

    /// Locate a status without decoding it when only its slot is needed.
    /// The caller owns retention validation for the enclosing operation.
    fn status_entry(
        &self,
        signature: Signature,
        highest_slot: Slot,
        oldest_slot: Slot,
    ) -> Option<(Slot, Box<[u8]>)> {
        self.rpc_api_metrics
            .num_get_transaction_status
            .fetch_add(1, Ordering::Relaxed);
        let mut iterator = self
            .transaction_status_cf
            .iter_current_index_filtered(IteratorMode::From(
                (signature, oldest_slot.max(1)),
                IteratorDirection::Forward,
            ));
        let ((found, slot), bytes) = iterator.next()?;
        (found == signature && slot <= highest_slot).then_some((slot, bytes))
    }

    pub fn read_transaction_status(
        &self,
        index: (Signature, Slot),
    ) -> LedgerResult<Option<TransactionStatusMeta>> {
        self.read_optional(index.1, || self.transaction_status(index))
    }

    /// Raw decoding; retention is validated by the enclosing logical read.
    fn transaction_status(
        &self,
        index: (Signature, Slot),
    ) -> LedgerResult<Option<TransactionStatusMeta>> {
        self.transaction_status_cf
            .get_protobuf(index)?
            .map(|meta| Self::convert_status(meta, index.1))
            .transpose()
    }

    fn convert_status(
        meta: generated::TransactionStatusMeta,
        slot: Slot,
    ) -> LedgerResult<TransactionStatusMeta> {
        meta.try_into().map_err(|err| {
            LedgerError::TransactionConversionError(format!(
                "invalid transaction status at slot {slot}: {err}"
            ))
        })
    }

    fn write_transaction_status(
        &self,
        slot: Slot,
        index: u32,
        signature: Signature,
        writable_keys: Vec<&Pubkey>,
        readonly_keys: Vec<&Pubkey>,
        status: TransactionStatusMeta,
    ) -> LedgerResult<()> {
        let status = status.into();

        for address in writable_keys {
            self.address_signatures_cf.put(
                (*address, slot, index, signature),
                &AddressSignatureMeta { writeable: true },
            )?;
            self.address_signatures_cf.try_increase_entry_counter(1);
        }
        for address in readonly_keys {
            self.address_signatures_cf.put(
                (*address, slot, index, signature),
                &AddressSignatureMeta { writeable: false },
            )?;
            self.address_signatures_cf.try_increase_entry_counter(1);
        }

        self.slot_signatures_cf.put((slot, index), &signature)?;
        self.slot_signatures_cf.try_increase_entry_counter(1);

        self.transaction_status_cf
            .put_protobuf((signature, slot), &status)?;
        self.transaction_status_cf.try_increase_entry_counter(1);

        if status.err.is_none() {
            try_increase_entry_counter(
                &self.transaction_successful_status_count,
                1,
            );
        } else {
            try_increase_entry_counter(
                &self.transaction_failed_status_count,
                1,
            );
        }
        Ok(())
    }

    /// Returns an iterator over all transaction statuses.
    /// The iterator item is an error if the status could not be decoded.
    ///
    /// NOTE: since the key is `(signature, slot)` the iterator cannot be used to
    ///       iterate in the order of slots
    ///
    /// - `iterator_mode` - The iterator mode to use for the search, defaults to [`IteratorMode::Start`]
    /// - `success` - If true, only successful transactions are returned,
    ///   otherwise only failed ones
    pub fn iter_transaction_statuses(
        &self,
        iterator_mode: Option<IteratorMode<(Signature, Slot)>>,
        success: bool,
    ) -> impl Iterator<
        Item = LedgerResult<(
            Slot,
            Signature,
            generated::TransactionStatusMeta,
        )>,
    > + '_ {
        let iterator_mode = iterator_mode.unwrap_or(IteratorMode::Start);
        self.transaction_status_cf
            .iter_protobuf(iterator_mode)
            .filter_map(move |res| {
                let ((signature, slot), status) = match res {
                    Ok(((signature, slot), status)) => {
                        ((signature, slot), status)
                    }
                    Err(err) => return Some(Err(err)),
                };
                let include = status.err.is_none() == success
                    && slot >= self.db.oldest_slot();
                if include {
                    Some(Ok((slot, signature, status)))
                } else {
                    None
                }
            })
    }

    pub fn count_transaction_status(&self) -> LedgerResult<i64> {
        self.transaction_status_cf.count_column_using_cache()
    }

    fn count_outcome_transaction_status(
        &self,
        success: bool,
    ) -> LedgerResult<i64> {
        let mut count = 0;
        for res in
            self.iter_transaction_statuses(Some(IteratorMode::Start), success)
        {
            match res {
                Ok(_) => count += 1,
                Err(err) => return Err(err),
            }
        }
        Ok(count)
    }

    pub fn count_transaction_successful_status(&self) -> LedgerResult<i64> {
        if self
            .transaction_status_cf
            .entry_counter
            .load(Ordering::Relaxed)
            == DIRTY_COUNT
        {
            let count = self.count_outcome_transaction_status(true)?;
            self.transaction_successful_status_count
                .store(count, Ordering::Relaxed);
            Ok(count)
        } else {
            Ok(self
                .transaction_successful_status_count
                .load(Ordering::Relaxed))
        }
    }

    pub fn count_transaction_failed_status(&self) -> LedgerResult<i64> {
        if self.transaction_failed_status_count.load(Ordering::Relaxed)
            == DIRTY_COUNT
        {
            let count = self.count_outcome_transaction_status(false)?;
            self.transaction_failed_status_count
                .store(count, Ordering::Relaxed);
            Ok(count)
        } else {
            Ok(self.transaction_failed_status_count.load(Ordering::Relaxed))
        }
    }

    // -----------------
    // Perf
    // -----------------
    pub fn get_recent_perf_samples(
        &self,
        num: usize,
    ) -> LedgerResult<Vec<(Slot, PerfSample)>> {
        let samples = self
            .db
            .iter::<cf::PerfSamples>(IteratorMode::End)?
            .take(num)
            .map(|(slot, data)| {
                deserialize::<PerfSample>(&data)
                    .map(|sample| (slot, sample))
                    .map_err(Into::into)
            });

        samples.collect()
    }

    pub fn write_perf_sample(
        &self,
        index: Slot,
        perf_sample: &PerfSample,
    ) -> LedgerResult<()> {
        // Always write as the current version.
        let bytes = serialize(perf_sample)
            .expect("`PerfSample` can be serialized with `bincode`");
        self.perf_samples_cf.put_bytes(index, &bytes)?;
        self.perf_samples_cf.try_increase_entry_counter(1);

        Ok(())
    }

    pub fn count_perf_samples(&self) -> LedgerResult<i64> {
        self.perf_samples_cf.count_column_using_cache()
    }

    pub fn read_slot_signature(
        &self,
        index: (Slot, u32),
    ) -> LedgerResult<Option<Signature>> {
        self.slot_signatures_cf.get(index)
    }

    /// Advances the shared read/compaction boundary without blocking readers.
    /// Only slots strictly below `slot` become eligible for cleanup. The caller
    /// must retain the latest persisted slot; lower boundaries are ignored.
    pub fn set_oldest_slot(&self, slot: Slot) {
        self.db.set_oldest_slot(slot);
    }

    pub fn delete_range_cf<C>(
        &self,
        from: C::Index,
        to: C::Index,
    ) -> LedgerResult<()>
    where
        C: Column + ColumnName,
        Self: HasColumn<C>,
    {
        <Ledger as HasColumn<C>>::column(self).delete_range(from, to)
    }

    pub fn compact_slot_range_cf<C: Column + ColumnName>(
        &self,
        from: Option<C::Index>,
        to: Option<C::Index>,
    ) {
        let mut measure = Measure::start("compaction");
        self.db.column::<C>().compact_range(from, to);
        measure.stop();

        info!("Compaction of column {} took: {}", C::NAME, measure);
    }

    /// Flushes all columns
    pub fn flush(&self) -> LedgerResult<()> {
        let cfs = [
            self.transaction_status_cf.handle(),
            self.address_signatures_cf.handle(),
            self.slot_signatures_cf.handle(),
            self.blocktime_cf.handle(),
            self.blockhash_cf.handle(),
            self.transaction_cf.handle(),
            self.transaction_memos_cf.handle(),
            self.perf_samples_cf.handle(),
        ];

        self.db
            .backend
            .flush_cfs_opt(&cfs, &FlushOptions::default())
    }

    /// Graceful db shutdown
    pub fn shutdown(&self, wait: bool) -> LedgerResult<()> {
        let _guard = MeasureGuard {
            measure: Measure::start("Ledger shutdown"),
            _timer: start_ledger_shutdown_timer(),
        };
        self.db.backend.db.cancel_all_background_work(wait);

        Ok(())
    }

    /// Lifts the background IO rate limit so shutdown flushes run at disk
    /// speed; call only after compactions have been stopped.
    pub fn lift_rate_limit(&self) {
        self.db.backend.lift_rate_limit();
    }

    /// Best-effort wait until no compactions are running, so lifting the
    /// rate limit cannot hand an in-flight compaction full disk bandwidth.
    /// Bounded: a throttled job mid-run may need minutes, and shutdown must
    /// not stall on it.
    pub fn wait_for_quiescent_compactions(&self, timeout: Duration) {
        const RUNNING_COMPACTIONS: &str = "rocksdb.num-running-compactions";
        let deadline = Instant::now() + timeout;
        loop {
            match self.db.backend.db.property_int_value(RUNNING_COMPACTIONS) {
                Ok(Some(running)) if running > 0 => {
                    if Instant::now() >= deadline {
                        warn!(
                            running,
                            "Compactions still running after quiesce timeout"
                        );
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
                Ok(_) => return,
                Err(err) => {
                    warn!(error = ?err, "Failed to query running compactions");
                    return;
                }
            }
        }
    }

    /// Disables automatic compactions on all columns; used at shutdown so
    /// flush-induced L0 growth cannot schedule compactions that would run
    /// unthrottled once the rate limit is lifted.
    pub fn disable_auto_compactions(&self) -> LedgerResult<()> {
        let cfs = [
            self.transaction_status_cf.handle(),
            self.address_signatures_cf.handle(),
            self.slot_signatures_cf.handle(),
            self.blocktime_cf.handle(),
            self.blockhash_cf.handle(),
            self.transaction_cf.handle(),
            self.transaction_memos_cf.handle(),
            self.perf_samples_cf.handle(),
        ];
        for cf in cfs {
            self.db
                .backend
                .db
                .set_options_cf(cf, &[("disable_auto_compactions", "true")])?;
        }
        Ok(())
    }

    /// Cancels manual compaction
    /// Here we utilize the internal of `disable_manual_compaction`
    /// Which not only disables future manual compaction,
    /// but also cancels all the running one
    pub fn cancel_manual_compactions(&self) {
        let _guard = MeasureGuard {
            measure: Measure::start("Compaction cancellation"),
            _timer: start_ledger_disable_compactions_timer(),
        };
        // Not exposed by the safe wrapper, reach through the C API
        unsafe {
            librocksdb_sys::rocksdb_disable_manual_compaction(
                self.db.backend.db.as_raw_ptr(),
            );
        }
    }

    /// Cached latest block data
    pub fn latest_block(&self) -> &LatestBlock {
        &self.latest_block
    }

    pub fn latest_blockhash(&self) -> BlockHash {
        self.latest_block.load().blockhash
    }
}

pub trait HasColumn<C>
where
    C: Column + ColumnName,
{
    fn column(&self) -> &LedgerColumn<C>;
    fn with_column<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&LedgerColumn<C>) -> R,
    {
        f(self.column())
    }
}

macro_rules! impl_has_column {
    ($cf_ty:ident, $field:ident) => {
        impl HasColumn<cf::$cf_ty> for Ledger {
            fn column(&self) -> &LedgerColumn<cf::$cf_ty> {
                &self.$field
            }
        }
    };
}

impl_has_column!(TransactionStatus, transaction_status_cf);
impl_has_column!(AddressSignatures, address_signatures_cf);
impl_has_column!(SlotSignatures, slot_signatures_cf);
impl_has_column!(Blocktime, blocktime_cf);
impl_has_column!(Blockhash, blockhash_cf);
impl_has_column!(Transaction, transaction_cf);
impl_has_column!(TransactionMemos, transaction_memos_cf);
impl_has_column!(PerfSamples, perf_samples_cf);

struct MeasureGuard {
    measure: Measure,
    _timer: HistogramTimer,
}

impl Drop for MeasureGuard {
    fn drop(&mut self) {
        self.measure.stop();
        // We print it in case metrics wouldn't have time to be scraped
        info!("{}", self.measure);
    }
}

// -----------------
// Tests
// -----------------
#[cfg(test)]
mod tests {
    use solana_clock::UnixTimestamp;
    use solana_instruction::error::InstructionError;
    use solana_keypair::Keypair;
    use solana_message::{
        compiled_instruction::CompiledInstruction, v0, MessageHeader,
        SimpleAddressLoader, VersionedMessage,
    };
    use solana_pubkey::Pubkey;
    use solana_signature::Signature;
    use solana_signer::Signer;
    use solana_transaction::sanitized::SanitizedTransaction;
    use solana_transaction_context::TransactionReturnData;
    use solana_transaction_error::{TransactionError, TransactionResult};
    use solana_transaction_status::{
        ConfirmedTransactionWithStatusMeta, InnerInstruction,
        InnerInstructions, TransactionStatusMeta, TransactionWithStatusMeta,
        VersionedTransactionWithStatusMeta,
    };
    use tempfile::{Builder, TempDir};
    use test_kit::init_logger;

    use super::*;

    pub fn get_ledger_path_from_name_auto_delete(name: &str) -> TempDir {
        let mut path = get_ledger_path_from_name(name);
        // path is a directory so .file_name() returns the last component of the path
        let last = path.file_name().unwrap().to_str().unwrap().to_string();
        path.pop();
        fs::create_dir_all(&path).unwrap();
        Builder::new()
            .prefix(&last)
            .rand_bytes(0)
            .tempdir_in(path)
            .unwrap()
    }

    pub fn get_ledger_path_from_name(name: &str) -> PathBuf {
        use std::env;
        let out_dir =
            env::var("FARF_DIR").unwrap_or_else(|_| "farf".to_string());
        let keypair = Keypair::new();

        let path = [
            out_dir,
            "ledger".to_string(),
            format!("{}-{}", name, keypair.pubkey()),
        ]
        .iter()
        .collect();

        // whack any possible collision
        let _ignored = fs::remove_dir_all(&path);

        path
    }

    #[macro_export]
    macro_rules! tmp_ledger_name {
        () => {
            &format!("{}-{}", file!(), line!())
        };
    }

    #[macro_export]
    macro_rules! get_tmp_ledger_path_auto_delete {
        () => {
            get_ledger_path_from_name_auto_delete(tmp_ledger_name!())
        };
    }

    fn create_transaction_status_meta(
        fee: u64,
    ) -> (TransactionStatusMeta, Vec<Pubkey>, Vec<Pubkey>) {
        let pre_balances_vec = vec![1, 2, 3];
        let post_balances_vec = vec![3, 2, 1];
        let inner_instructions_vec = vec![InnerInstructions {
            index: 0,
            instructions: vec![InnerInstruction {
                instruction: CompiledInstruction::new(1, &(), vec![0]),
                stack_height: Some(2),
            }],
        }];
        let log_messages_vec = vec![String::from("Test message\n")];
        let pre_token_balances_vec = vec![];
        let post_token_balances_vec = vec![];
        let rewards_vec = vec![];
        let writable_keys = vec![Pubkey::new_unique()];
        let readonly_keys = vec![Pubkey::new_unique()];
        let test_return_data = TransactionReturnData {
            program_id: Pubkey::new_unique(),
            data: vec![1, 2, 3],
        };
        let compute_units_consumed_1 = Some(3812649u64);

        (
            TransactionStatusMeta {
                status: TransactionResult::Err(
                    TransactionError::InstructionError(
                        99,
                        InstructionError::Custom(69),
                    ),
                ),
                fee,
                pre_balances: pre_balances_vec.clone(),
                post_balances: post_balances_vec.clone(),
                inner_instructions: Some(inner_instructions_vec.clone()),
                log_messages: Some(log_messages_vec.clone()),
                pre_token_balances: Some(pre_token_balances_vec.clone()),
                post_token_balances: Some(post_token_balances_vec.clone()),
                rewards: Some(rewards_vec.clone()),
                loaded_addresses: Default::default(),
                return_data: Some(test_return_data.clone()),
                compute_units_consumed: compute_units_consumed_1,
                cost_units: None,
            },
            writable_keys,
            readonly_keys,
        )
    }

    fn create_confirmed_transaction(
        slot: Slot,
        fee: u64,
        block_time: Option<UnixTimestamp>,
        tx_signatures: Option<Vec<Signature>>,
    ) -> (ConfirmedTransactionWithStatusMeta, SanitizedTransaction) {
        let (meta, writable_keys, readonly_keys) =
            create_transaction_status_meta(fee);
        let num_readonly_unsigned_accounts = readonly_keys.len() as u8 - 1;
        let signatures = tx_signatures.unwrap_or_else(|| {
            vec![Signature::new_unique(), Signature::new_unique()]
        });
        let msg = v0::Message {
            account_keys: [writable_keys, readonly_keys].concat(),
            header: MessageHeader {
                num_required_signatures: signatures.len() as u8,
                num_readonly_signed_accounts: 1,
                num_readonly_unsigned_accounts,
            },
            ..Default::default()
        };
        let transaction = VersionedTransaction {
            signatures,
            message: VersionedMessage::V0(msg),
        };
        let tx_with_meta = VersionedTransactionWithStatusMeta {
            transaction: transaction.clone(),
            meta: meta.clone(),
        };
        let tx_with_meta = TransactionWithStatusMeta::Complete(tx_with_meta);

        let sanitized_transaction = SanitizedTransaction::try_new(
            transaction
                .try_into()
                .map_err(|e| {
                    error!(error = ?e, "VersionedTransaction::try_into failed")
                })
                .unwrap(),
            Default::default(),
            false,
            SimpleAddressLoader::Enabled(meta.loaded_addresses.clone()),
            &Default::default(),
        )
        .map_err(|e| error!(error = ?e, "SanitizedTransaction::try_new failed"))
        .unwrap();

        (
            ConfirmedTransactionWithStatusMeta {
                slot,
                block_time,
                tx_with_meta,
                index: 0,
            },
            sanitized_transaction,
        )
    }

    macro_rules! keys_as_ref {
        ($keys:expr) => {
            $keys.iter().collect()
        };
    }

    #[test]
    fn test_persist_transaction_status() {
        init_logger!();
        let ledger_path = get_tmp_ledger_path_auto_delete!();
        let store = Ledger::open(ledger_path.path()).unwrap();

        // First Case
        {
            let (signature, slot) = (Signature::default(), 0);

            // result not found
            assert!(store
                .read_transaction_status((Signature::default(), 0))
                .unwrap()
                .is_none());

            // insert value
            let (meta, writable_keys, readonly_keys) =
                create_transaction_status_meta(5);
            assert!(store
                .write_transaction_status(
                    slot,
                    0,
                    signature,
                    keys_as_ref!(writable_keys),
                    keys_as_ref!(readonly_keys),
                    meta.clone(),
                )
                .is_ok());

            // result found
            let found = store
                .read_transaction_status((signature, slot))
                .unwrap()
                .unwrap();
            assert_eq!(found, meta);
        }

        // Second Case
        {
            // insert value
            let (signature, slot) = (Signature::from([2u8; 64]), 9);
            let (meta, writable_keys, readonly_keys) =
                create_transaction_status_meta(9);
            assert!(store
                .write_transaction_status(
                    slot,
                    0,
                    signature,
                    keys_as_ref!(writable_keys),
                    keys_as_ref!(readonly_keys),
                    meta.clone(),
                )
                .is_ok());

            // result found
            let found = store
                .read_transaction_status((signature, slot))
                .unwrap()
                .unwrap();
            assert_eq!(found, meta);
        }
    }

    #[test]
    fn test_get_transaction_status_by_signature() {
        init_logger!();
        let ledger_path = get_tmp_ledger_path_auto_delete!();
        let store = Ledger::open(ledger_path.path()).unwrap();

        let (sig_uno, slot_uno) = (Signature::default(), 10);
        let (sig_dos, slot_dos) = (Signature::from([2u8; 64]), 20);

        // result not found
        assert!(store
            .read_transaction_status((Signature::default(), slot_uno))
            .unwrap()
            .is_none());

        // insert value
        let (status_uno, writable_keys, readonly_keys) =
            create_transaction_status_meta(5);
        assert!(store
            .write_transaction_status(
                slot_uno,
                0,
                sig_uno,
                keys_as_ref!(writable_keys),
                keys_as_ref!(readonly_keys),
                status_uno.clone(),
            )
            .is_ok());

        // Finds by matching signature
        {
            let (slot, status) = store
                .get_transaction_status(sig_uno, slot_uno + 5)
                .unwrap()
                .unwrap();
            assert_eq!(slot, slot_uno);
            assert_eq!(status, status_uno);

            // Does not find it by other signature
            assert!(store
                .get_transaction_status(sig_dos, slot_uno)
                .unwrap()
                .is_none());
        }

        // Add a status for the other signature
        let (status_dos, writable_keys, readonly_keys) =
            create_transaction_status_meta(5);
        assert!(store
            .write_transaction_status(
                slot_dos,
                0,
                sig_dos,
                keys_as_ref!(writable_keys),
                keys_as_ref!(readonly_keys),
                status_dos.clone(),
            )
            .is_ok());

        // First still there
        {
            let (slot, status) = store
                .get_transaction_status(sig_uno, slot_uno)
                .unwrap()
                .unwrap();
            assert_eq!(slot, slot_uno);
            assert_eq!(status, status_uno);
        }

        // Second one is found now as well
        {
            let (slot, status) = store
                .get_transaction_status(sig_dos, slot_dos)
                .unwrap()
                .unwrap();
            assert_eq!(slot, slot_dos);
            assert_eq!(status, status_dos);
        }
    }

    #[test]
    fn test_get_complete_transaction_by_signature() {
        init_logger!();
        let ledger_path = get_tmp_ledger_path_auto_delete!();
        let store = Ledger::open(ledger_path.path()).unwrap();

        let (sig_uno, slot_uno, block_time_uno, block_hash_uno) =
            (Signature::default(), 10, 100, Hash::new_unique());
        let (sig_dos, slot_dos, block_time_dos, block_hash_dos) =
            (Signature::from([2u8; 64]), 20, 200, Hash::new_unique());

        let (tx_uno, sanitized_uno) = create_confirmed_transaction(
            slot_uno,
            5,
            Some(block_time_uno),
            None,
        );

        let (tx_dos, sanitized_dos) = create_confirmed_transaction(
            slot_dos,
            9,
            Some(block_time_dos),
            None,
        );

        // 0. Neither transaction is in the store
        assert!(store
            .get_complete_transaction(sig_uno, 0)
            .unwrap()
            .is_none());
        assert!(store
            .get_complete_transaction(sig_dos, 0)
            .unwrap()
            .is_none());

        // 1. Write first transaction and block time for relevant slot
        let versioned_uno = sanitized_uno.to_versioned_transaction();
        let encoded_uno = serialize(&versioned_uno).unwrap();
        let locks_uno = sanitized_uno.get_account_locks_unchecked();
        assert!(store
            .write_transaction(
                sig_uno,
                slot_uno,
                0,
                locks_uno.writable,
                locks_uno.readonly,
                &encoded_uno,
                tx_uno.tx_with_meta.get_status_meta().unwrap(),
            )
            .is_ok());
        assert!(store
            .write_block(LatestBlockInner::new(
                slot_uno,
                block_hash_uno,
                block_time_uno
            ))
            .is_ok());

        // Get first transaction by signature providing high enough slot
        let tx = store
            .get_complete_transaction(sig_uno, slot_uno)
            .unwrap()
            .unwrap();
        assert_eq!(tx, tx_uno);

        // Get first transaction by signature providing slot that's too low
        assert!(store
            .get_complete_transaction(sig_uno, slot_uno - 1)
            .unwrap()
            .is_none());

        // 2. Write second transaction and block time for relevant slot
        let versioned_dos = sanitized_dos.to_versioned_transaction();
        let encoded_dos = serialize(&versioned_dos).unwrap();
        let locks_dos = sanitized_dos.get_account_locks_unchecked();
        assert!(store
            .write_transaction(
                sig_dos,
                slot_dos,
                0,
                locks_dos.writable,
                locks_dos.readonly,
                &encoded_dos,
                tx_dos.tx_with_meta.get_status_meta().unwrap(),
            )
            .is_ok());
        assert!(store
            .write_block(LatestBlockInner::new(
                slot_dos,
                block_hash_dos,
                block_time_dos
            ))
            .is_ok());

        // Get second transaction by signature providing slot at which it was stored
        let tx = store
            .get_complete_transaction(sig_dos, slot_dos)
            .unwrap()
            .unwrap();
        assert_eq!(tx, tx_dos);
    }

    #[test]
    fn test_find_address_signatures_no_intra_slot_limits() {
        init_logger!();
        let ledger_path = get_tmp_ledger_path_auto_delete!();
        let store = Ledger::open(ledger_path.path()).unwrap();

        // 1. Add some transaction statuses
        let (signature_uno, slot_uno) = (Signature::new_unique(), 10);
        store
            .write_block(LatestBlockInner::new(
                slot_uno,
                BlockHash::new_unique(),
                0,
            ))
            .unwrap();

        let (read_uno, write_uno) = {
            let (meta, writable_keys, readonly_keys) =
                create_transaction_status_meta(5);
            let read_uno = readonly_keys[0];
            let write_uno = writable_keys[0];
            assert!(store
                .write_transaction_status(
                    slot_uno,
                    0,
                    signature_uno,
                    keys_as_ref!(writable_keys),
                    keys_as_ref!(readonly_keys),
                    meta.clone(),
                )
                .is_ok());
            (read_uno, write_uno)
        };

        let (signature_dos, slot_dos) = (Signature::new_unique(), 20);
        store
            .write_block(LatestBlockInner::new(
                slot_dos,
                BlockHash::new_unique(),
                0,
            ))
            .unwrap();
        let signature_dos_2 = Signature::new_unique();
        let (read_dos, write_dos) = {
            let (meta, mut writable_keys, mut readonly_keys) =
                create_transaction_status_meta(5);
            let read_dos = readonly_keys[0];
            let write_dos = writable_keys[0];
            readonly_keys.push(read_uno);
            writable_keys.push(write_uno);
            assert!(store
                .write_transaction_status(
                    slot_dos,
                    0,
                    signature_dos,
                    keys_as_ref!(writable_keys),
                    keys_as_ref!(readonly_keys),
                    meta.clone(),
                )
                .is_ok());

            // read_dos and write_dos are part of another transaction in the same slot
            // signature_dos_2 at times is captured via intra slot logic, but the focus
            // of this method is not intra slot
            let (meta, mut writable_keys, mut readonly_keys) =
                create_transaction_status_meta(8);
            readonly_keys.push(read_dos);
            writable_keys.push(write_dos);
            assert!(store
                .write_transaction_status(
                    slot_dos,
                    1,
                    signature_dos_2,
                    keys_as_ref!(writable_keys),
                    keys_as_ref!(readonly_keys),
                    meta.clone(),
                )
                .is_ok());

            (read_dos, write_dos)
        };

        let (signature_tres, slot_tres) = (Signature::new_unique(), 30);
        store
            .write_block(LatestBlockInner::new(
                slot_tres,
                BlockHash::new_unique(),
                0,
            ))
            .unwrap();
        let (_read_tres, _write_tres) = {
            let (meta, mut writable_keys, mut readonly_keys) =
                create_transaction_status_meta(5);
            let read_tres = readonly_keys[0];
            let write_tres = writable_keys[0];
            readonly_keys.push(read_uno);
            writable_keys.push(write_uno);
            readonly_keys.push(read_dos);
            writable_keys.push(write_dos);

            assert!(store
                .write_transaction_status(
                    slot_tres,
                    0,
                    signature_tres,
                    keys_as_ref!(writable_keys),
                    keys_as_ref!(readonly_keys),
                    meta.clone(),
                )
                .is_ok());
            (read_tres, write_tres)
        };

        let (signature_cuatro, slot_cuatro) = (Signature::new_unique(), 31);
        store
            .write_block(LatestBlockInner::new(
                slot_cuatro,
                BlockHash::new_unique(),
                0,
            ))
            .unwrap();
        let (read_cuatro, _write_cuatro) = {
            let (meta, writable_keys, readonly_keys) =
                create_transaction_status_meta(5);
            let read_cuatro = readonly_keys[0];
            let write_cuatro = writable_keys[0];
            assert!(store
                .write_transaction_status(
                    slot_cuatro,
                    0,
                    signature_cuatro,
                    keys_as_ref!(writable_keys),
                    keys_as_ref!(readonly_keys),
                    meta.clone(),
                )
                .is_ok());
            (read_cuatro, write_cuatro)
        };

        let (signature_cinco, slot_cinco) = (Signature::new_unique(), 31);
        store
            .write_block(LatestBlockInner::new(
                slot_cinco,
                BlockHash::new_unique(),
                0,
            ))
            .unwrap();
        let (_read_cinco, _write_cinco) = {
            let (meta, writable_keys, readonly_keys) =
                create_transaction_status_meta(5);
            let read_cinco = readonly_keys[0];
            let write_cinco = writable_keys[0];
            assert!(store
                .write_transaction_status(
                    slot_cinco,
                    1,
                    signature_cinco,
                    keys_as_ref!(writable_keys),
                    keys_as_ref!(readonly_keys),
                    meta.clone(),
                )
                .is_ok());
            (read_cinco, write_cinco)
        };

        let (signature_seis, slot_seis) = (Signature::new_unique(), 32);
        store
            .write_block(LatestBlockInner::new(
                slot_seis,
                BlockHash::new_unique(),
                0,
            ))
            .unwrap();
        let (_read_seis, _write_seis) = {
            let (meta, mut writable_keys, mut readonly_keys) =
                create_transaction_status_meta(5);
            let read_seis = readonly_keys[0];
            let write_seis = writable_keys[0];
            readonly_keys.push(read_uno);
            writable_keys.push(write_uno);
            assert!(store
                .write_transaction_status(
                    slot_seis,
                    0,
                    signature_seis,
                    keys_as_ref!(writable_keys),
                    keys_as_ref!(readonly_keys),
                    meta.clone(),
                )
                .is_ok());
            (read_seis, write_seis)
        };

        // Now we have the following addresses be part of the following transactions
        //
        //   signature_uno   : read_uno, write_uno
        //   signature_dos   : read_dos, write_dos, read_uno, write_uno
        //   signature_dos_2 : read_dos, write_dos
        //   signature_tres  : read_tres, write_tres, read_dos, write_dos, read_uno, write_uno
        //   signature_cuatro: read_cuatro, write_cuatro
        //   signature_cinco : read_cinco, write_cinco
        //   signature_seis  : read_seis, write_seis, read_uno, write_uno
        //
        // Grouped by address:
        //
        //  read_uno | write_uno      : signature_uno, signature_dos, signature_tres, signature_seis
        //  read_dos | write_dos      : signature_dos, signature_dos_2, signature_tres
        //  read_tres | write_tres    : signature_tres
        //  read_cuatro | write_cuatro: signature_cuatro
        //  read_cinco | write_cinco  : signature_cinco
        //  read_seis | write_seis    : signature_seis

        // 2. Fill in block times
        assert!(store
            .write_block(LatestBlockInner::new(slot_uno, Hash::new_unique(), 1))
            .is_ok());
        assert!(store
            .write_block(LatestBlockInner::new(slot_dos, Hash::new_unique(), 2))
            .is_ok());
        assert!(store
            .write_block(LatestBlockInner::new(
                slot_tres,
                Hash::new_unique(),
                3
            ))
            .is_ok());
        assert!(store
            .write_block(LatestBlockInner::new(
                slot_cuatro,
                Hash::new_unique(),
                4
            ))
            .is_ok());
        assert!(store
            .write_block(LatestBlockInner::new(
                slot_cinco,
                Hash::new_unique(),
                5
            ))
            .is_ok());
        assert!(store
            .write_block(LatestBlockInner::new(
                slot_seis,
                Hash::new_unique(),
                6
            ))
            .is_ok());

        // 3. Find signatures for address with default limits
        let res = store
            .get_confirmed_signatures_for_address(
                read_cuatro,
                slot_seis,
                None,
                None,
                1000,
            )
            .unwrap();
        assert!(!res.found_upper);
        assert_eq!(res.infos.len(), 1);
        assert_eq!(
            res.infos[0],
            ConfirmedTransactionStatusWithSignature {
                signature: signature_cuatro,
                slot: 31,
                err: Some(TransactionError::InstructionError(
                    99,
                    InstructionError::Custom(69)
                )),
                memo: None,
                block_time: Some(5),
                index: 0,
            }
        );

        // 4. Find signatures with before/until configs
        fn extract(
            infos: Vec<ConfirmedTransactionStatusWithSignature>,
        ) -> Vec<(Slot, Signature)> {
            infos.into_iter().map(|x| (x.slot, x.signature)).collect()
        }

        // No before/until
        {
            let sigs = extract(
                store
                    .get_confirmed_signatures_for_address(
                        read_uno, slot_seis, None, None, 1000,
                    )
                    .unwrap()
                    .infos,
            );
            assert!(!res.found_upper);
            assert_eq!(
                sigs,
                vec![
                    (slot_seis, signature_seis),
                    (slot_tres, signature_tres),
                    (slot_dos, signature_dos),
                    (slot_uno, signature_uno),
                ]
            );
        }

        // Before configured only
        {
            // Before signature tres
            let res = store
                .get_confirmed_signatures_for_address(
                    read_uno,
                    slot_seis,
                    Some(signature_tres),
                    None,
                    1000,
                )
                .unwrap();
            assert!(res.found_upper);
            assert_eq!(
                extract(res.infos.clone()),
                vec![(slot_dos, signature_dos), (slot_uno, signature_uno),]
            );

            // Before signature cuatro
            let res = store
                .get_confirmed_signatures_for_address(
                    read_uno,
                    slot_seis,
                    Some(signature_cuatro),
                    None,
                    1000,
                )
                .unwrap();
            assert!(res.found_upper);
            assert_eq!(
                extract(res.infos.clone()),
                vec![
                    (slot_tres, signature_tres),
                    (slot_dos, signature_dos),
                    (slot_uno, signature_uno),
                ]
            );
        }

        // Until configured only
        {
            // Until signature tres
            let res = store
                .get_confirmed_signatures_for_address(
                    read_uno,
                    slot_seis,
                    None,
                    Some(signature_tres),
                    1000,
                )
                .unwrap();
            assert!(res.found_lower);

            assert_eq!(
                extract(res.infos.clone()),
                vec![(slot_seis, signature_seis),]
            );

            // Until signature dos
            let res = store
                .get_confirmed_signatures_for_address(
                    read_uno,
                    slot_seis,
                    None,
                    Some(signature_dos),
                    1000,
                )
                .unwrap();
            assert!(res.found_lower);

            assert_eq!(
                extract(res.infos.clone()),
                vec![(slot_seis, signature_seis), (slot_tres, signature_tres),]
            );
        }
        // Before/Until configured
        {
            let res = store
                .get_confirmed_signatures_for_address(
                    read_dos,
                    slot_seis,
                    Some(signature_cuatro),
                    Some(signature_dos),
                    1000,
                )
                .unwrap();
            assert!(res.found_upper);
            assert!(res.found_lower);

            assert_eq!(
                extract(res.infos.clone()),
                vec![(slot_tres, signature_tres), (slot_dos, signature_dos_2)]
            );
        }

        // Highest Slot lower than Upper Limit
        {
            let res = store
                .get_confirmed_signatures_for_address(
                    read_uno,
                    slot_dos,
                    Some(signature_cuatro),
                    None,
                    1000,
                )
                .unwrap();
            assert!(res.found_upper);

            assert_eq!(
                extract(res.infos.clone()),
                vec![(slot_dos, signature_dos), (slot_uno, signature_uno)]
            );
        }
    }

    #[test]
    fn test_find_address_signatures_intra_slot_limits() {
        init_logger!();
        let ledger_path = get_tmp_ledger_path_auto_delete!();
        let store = Ledger::open(ledger_path.path()).unwrap();

        // Add the signatures such that we get the following all include the same address
        // for simplicity:
        //
        // Slot1: sig1, sig2, sig3
        // Slot2: sig4, sig5
        // Slot3: sig6, sig7, sig8

        // 1. Add transaction statuses
        let (sig1, slot1) = (Signature::new_unique(), 10);
        let sig2 = Signature::new_unique();
        let sig3 = Signature::new_unique();

        let (sig4, slot2) = (Signature::new_unique(), 11);
        let sig5 = Signature::new_unique();

        let (sig6, slot3) = (Signature::new_unique(), 12);
        let sig7 = Signature::new_unique();
        let sig8 = Signature::new_unique();

        let mut current_slot = 0;
        let mut current_index = 0;
        let read_uno = {
            let (meta, writable_keys, readonly_keys) =
                create_transaction_status_meta(5);
            let read_uno = readonly_keys[0];
            assert!(store
                .write_block(LatestBlockInner::new(
                    slot1,
                    Hash::new_unique(),
                    1
                ))
                .is_ok());
            assert!(store
                .write_block(LatestBlockInner::new(
                    slot2,
                    Hash::new_unique(),
                    2
                ))
                .is_ok());
            assert!(store
                .write_block(LatestBlockInner::new(
                    slot3,
                    Hash::new_unique(),
                    3
                ))
                .is_ok());
            for (slot, signature) in &[
                (slot1, sig1),
                (slot1, sig2),
                (slot1, sig3),
                (slot2, sig4),
                (slot2, sig5),
                (slot3, sig6),
                (slot3, sig7),
                (slot3, sig8),
            ] {
                if *slot != current_slot {
                    current_slot = *slot;
                    current_index = 0;
                }
                assert!(store
                    .write_transaction_status(
                        *slot,
                        current_index,
                        *signature,
                        keys_as_ref!(writable_keys.clone()),
                        keys_as_ref!(readonly_keys.clone()),
                        meta.clone(),
                    )
                    .is_ok());
                current_index += 1;
            }

            read_uno
        };

        fn extract(
            infos: Vec<ConfirmedTransactionStatusWithSignature>,
        ) -> Vec<(Slot, Signature)> {
            infos.into_iter().map(|x| (x.slot, x.signature)).collect()
        }

        // Find anything older than sig3 (2, 1) in same slot
        let res = store
            .get_confirmed_signatures_for_address(
                read_uno,
                slot1,
                Some(sig3),
                None,
                1000,
            )
            .unwrap();
        assert_eq!(
            extract(res.infos.clone()),
            vec![(slot1, sig2), (slot1, sig1),]
        );
        // Find anything older than sig2 (1) in same slot
        let res = store
            .get_confirmed_signatures_for_address(
                read_uno,
                slot1,
                Some(sig2),
                None,
                1000,
            )
            .unwrap();
        assert_eq!(extract(res.infos.clone()), vec![(slot1, sig1),]);

        // Find anything newer than sig6 (8, 7) in same slot
        let res = store
            .get_confirmed_signatures_for_address(
                read_uno,
                slot3,
                None,
                Some(sig6),
                1000,
            )
            .unwrap();
        assert_eq!(
            extract(res.infos.clone()),
            vec![(slot3, sig8), (slot3, sig7),]
        );

        // Find anything newer than sig7 (8) in same slot
        let res = store
            .get_confirmed_signatures_for_address(
                read_uno,
                slot3,
                None,
                Some(sig7),
                1000,
            )
            .unwrap();
        assert_eq!(extract(res.infos.clone()), vec![(slot3, sig8)]);

        // Find anything newer than sig4 across slots
        let res = store
            .get_confirmed_signatures_for_address(
                read_uno,
                slot3,
                None,
                Some(sig4),
                1000,
            )
            .unwrap();
        assert_eq!(
            extract(res.infos.clone()),
            vec![(slot3, sig8), (slot3, sig7), (slot3, sig6), (slot2, sig5),]
        );

        // Find anyting newer than sig4 across slots, however highest_slot
        // excludes any of them
        let res = store
            .get_confirmed_signatures_for_address(
                read_uno,
                slot1,
                None,
                Some(sig4),
                1000,
            )
            .unwrap();
        assert!(res.found_lower);
        assert_eq!(extract(res.infos.clone()), vec![]);

        // Find anything older than sig5 across slots
        let res = store
            .get_confirmed_signatures_for_address(
                read_uno,
                slot3,
                Some(sig5),
                None,
                1000,
            )
            .unwrap();
        assert_eq!(
            extract(res.infos.clone()),
            vec![(slot2, sig4), (slot1, sig3), (slot1, sig2), (slot1, sig1),]
        );

        // Find anything older than sig5 across slots, however highest
        // slot exludes slot2
        let res = store
            .get_confirmed_signatures_for_address(
                read_uno,
                slot1,
                Some(sig5),
                None,
                1000,
            )
            .unwrap();
        assert_eq!(
            extract(res.infos.clone()),
            vec![(slot1, sig3), (slot1, sig2), (slot1, sig1),]
        );

        // Find anything in between sig2 and sig7
        let res = store
            .get_confirmed_signatures_for_address(
                read_uno,
                slot3,
                Some(sig7),
                Some(sig2),
                1000,
            )
            .unwrap();
        assert_eq!(
            extract(res.infos.clone()),
            vec![(slot3, sig6), (slot2, sig5), (slot2, sig4), (slot1, sig3),]
        );

        // Find anything in between sig2 and sig7, but highest slot
        // exlcudes slot3
        let res = store
            .get_confirmed_signatures_for_address(
                read_uno,
                slot2,
                Some(sig7),
                Some(sig2),
                1000,
            )
            .unwrap();
        assert_eq!(
            extract(res.infos.clone()),
            vec![(slot2, sig5), (slot2, sig4), (slot1, sig3),]
        );

        // Find anything in between sig2 and sig7, but limit is 2
        let res = store
            .get_confirmed_signatures_for_address(
                read_uno,
                slot3,
                Some(sig7),
                Some(sig2),
                2,
            )
            .unwrap();
        assert_eq!(
            extract(res.infos.clone()),
            vec![(slot3, sig6), (slot2, sig5),]
        );

        // Find anything in between sig2 and sig7, but limit is 2 and
        // highest_slot forces us to start at slot2
        let res = store
            .get_confirmed_signatures_for_address(
                read_uno,
                slot2,
                Some(sig7),
                Some(sig2),
                2,
            )
            .unwrap();
        assert_eq!(
            extract(res.infos.clone()),
            vec![(slot2, sig5), (slot2, sig4)]
        );
    }

    #[test]
    fn test_get_confirmed_signatures_with_memos() {
        init_logger!();
        let ledger_path = get_tmp_ledger_path_auto_delete!();
        let store = Ledger::open(ledger_path.path()).unwrap();

        let (sig_uno, slot_uno) = (Signature::new_unique(), 10);
        let (sig_dos, slot_dos) = (Signature::new_unique(), 10);

        let (tx_uno, sanitized_uno) =
            create_confirmed_transaction(slot_uno, 5, Some(100), None);
        let (tx_dos, sanitized_dos) =
            create_confirmed_transaction(slot_dos, 5, Some(100), None);

        // 1. Write transactions and block time + memo for relevant slot
        {
            let versioned_uno = sanitized_uno.to_versioned_transaction();
            let encoded_uno = serialize(&versioned_uno).unwrap();
            let locks_uno = sanitized_uno.get_account_locks_unchecked();
            assert!(store
                .write_transaction(
                    sig_uno,
                    slot_uno,
                    0,
                    locks_uno.writable,
                    locks_uno.readonly,
                    &encoded_uno,
                    tx_uno.tx_with_meta.get_status_meta().unwrap(),
                )
                .is_ok());

            assert!(store
                .write_block(LatestBlockInner::new(
                    slot_uno,
                    Hash::new_unique(),
                    100
                ))
                .is_ok());

            assert!(store
                .write_transaction_memos(
                    &sig_uno,
                    slot_uno,
                    "Test Uno Memo".to_string()
                )
                .is_ok());
        }

        {
            let versioned_dos = sanitized_dos.to_versioned_transaction();
            let encoded_dos = serialize(&versioned_dos).unwrap();
            let locks_dos = sanitized_dos.get_account_locks_unchecked();
            assert!(store
                .write_transaction(
                    sig_dos,
                    slot_dos,
                    0,
                    locks_dos.writable,
                    locks_dos.readonly,
                    &encoded_dos,
                    tx_dos.tx_with_meta.get_status_meta().unwrap(),
                )
                .is_ok());
            assert!(store
                .write_block(LatestBlockInner::new(
                    slot_dos,
                    Hash::new_unique(),
                    100
                ))
                .is_ok());
            assert!(store
                .write_transaction_memos(
                    &sig_dos,
                    slot_dos,
                    "Test Dos Memo".to_string()
                )
                .is_ok());
        }

        // 2. Retrieve Confirmed Signatures and check for Memos
        {
            // Get first one directly
            let memo = store.read_transaction_memos(sig_uno, slot_uno).unwrap();
            assert_eq!(memo, Some("Test Uno Memo".to_string()));

            // Make sure it's included when we get confirmed signatures
            let address_uno = sanitized_uno.message().account_keys()[0];
            let sig_info_uno = &store
                .get_confirmed_signatures_for_address(
                    address_uno,
                    slot_uno,
                    None,
                    None,
                    1000,
                )
                .unwrap()
                .infos[0];
            assert_eq!(sig_info_uno.memo, Some("Test Uno Memo".to_string()));
        }

        {
            // Get second one directly
            let memo = store.read_transaction_memos(sig_dos, slot_dos).unwrap();
            assert_eq!(memo, Some("Test Dos Memo".to_string()));

            // Make sure it's included when we get confirmed signatures
            let address_dos = sanitized_dos.message().account_keys()[0];
            let sig_info_dos = &store
                .get_confirmed_signatures_for_address(
                    address_dos,
                    slot_dos,
                    None,
                    None,
                    1000,
                )
                .unwrap()
                .infos[0];
            assert_eq!(sig_info_dos.memo, Some("Test Dos Memo".to_string()));
        }
    }

    #[test]
    fn test_verify_transaction_signature() {
        init_logger!();
        let ledger_path = get_tmp_ledger_path_auto_delete!();
        let store = Ledger::open(ledger_path.path()).unwrap();

        // Create a properly signed transaction
        let from_keypair = Keypair::new();
        let to = Pubkey::new_unique();
        let blockhash = Hash::new_unique();
        let tx = solana_system_transaction::transfer(
            &from_keypair,
            &to,
            42,
            blockhash,
        );
        let versioned_tx = VersionedTransaction::from(tx);
        let signature = versioned_tx.signatures[0];
        let slot = 10u64;

        // Encode and write the transaction to the ledger
        let encoded = serialize(&versioned_tx).unwrap();
        let (meta, _, _) = create_transaction_status_meta(5);
        let writable_keys = versioned_tx.message.static_account_keys()[..1]
            .iter()
            .collect();
        let readonly_keys = versioned_tx.message.static_account_keys()[1..]
            .iter()
            .collect();
        store
            .write_transaction(
                signature,
                slot,
                0,
                writable_keys,
                readonly_keys,
                &encoded,
                meta,
            )
            .unwrap();
        store
            .write_block(LatestBlockInner::new(slot, Hash::new_unique(), 100))
            .unwrap();

        // Verify a properly signed transaction returns Some(true)
        let result = store.verify_transaction_signature(&signature).unwrap();
        assert_eq!(result, Some(true));

        // Verify a non-existent signature returns None
        let random_sig = Signature::new_unique();
        let result = store.verify_transaction_signature(&random_sig).unwrap();
        assert_eq!(result, None);
    }

    #[test]
    fn test_verify_transaction_signature_not_found() {
        init_logger!();
        let ledger_path = get_tmp_ledger_path_auto_delete!();
        let store = Ledger::open(ledger_path.path()).unwrap();

        // Query an empty ledger — no transaction exists
        let sig = Signature::new_unique();
        let result = store.verify_transaction_signature(&sig).unwrap();
        assert_eq!(result, None);
    }

    #[test]
    fn test_verify_transaction_signature_invalid() {
        init_logger!();
        let ledger_path = get_tmp_ledger_path_auto_delete!();
        let store = Ledger::open(ledger_path.path()).unwrap();

        // Build a transaction with a bogus signature (not matching keypair)
        let from_keypair = Keypair::new();
        let to = Pubkey::new_unique();
        let blockhash = Hash::new_unique();
        let tx = solana_system_transaction::transfer(
            &from_keypair,
            &to,
            42,
            blockhash,
        );
        let mut versioned_tx = VersionedTransaction::from(tx);

        // Corrupt the signature so verification will fail
        let real_sig = versioned_tx.signatures[0];
        versioned_tx.signatures[0] = Signature::new_unique();
        let bad_sig = versioned_tx.signatures[0];

        let encoded = serialize(&versioned_tx).unwrap();
        let (meta, _, _) = create_transaction_status_meta(5);
        let writable_keys = versioned_tx.message.static_account_keys()[..1]
            .iter()
            .collect();
        let readonly_keys = versioned_tx.message.static_account_keys()[1..]
            .iter()
            .collect();
        let slot = 10u64;
        store
            .write_transaction(
                bad_sig,
                slot,
                0,
                writable_keys,
                readonly_keys,
                &encoded,
                meta,
            )
            .unwrap();
        store
            .write_block(LatestBlockInner::new(slot, Hash::new_unique(), 100))
            .unwrap();

        // The corrupted signature should fail verification
        let result = store.verify_transaction_signature(&bad_sig).unwrap();
        assert_eq!(result, Some(false));

        // The original valid signature is not in the ledger
        let result = store.verify_transaction_signature(&real_sig).unwrap();
        assert_eq!(result, None);
    }

    /// Records a bare `(slot, index) -> signature` mapping in
    /// `slot_signatures_cf`, the way replayed transactions do via
    /// `record_transaction`. Keeps these focused tests independent of full
    /// transaction encoding.
    fn record_signature(
        store: &Ledger,
        slot: Slot,
        index: u32,
        sig: Signature,
    ) {
        let (meta, _writable, _readonly) = create_transaction_status_meta(5);
        store
            .write_transaction_status(slot, index, sig, vec![], vec![], meta)
            .unwrap();
    }

    #[test]
    fn test_get_transaction_signatures_for_slot_ascending() {
        init_logger!();
        let ledger_path = get_tmp_ledger_path_auto_delete!();
        let store = Ledger::open(ledger_path.path()).unwrap();

        let slot = 10;
        let sig0 = Signature::from([0u8; 64]);
        let sig1 = Signature::from([1u8; 64]);
        let sig2 = Signature::from([2u8; 64]);
        // Insert out of index order to prove sorting is by key, not insertion.
        record_signature(&store, slot, 2, sig2);
        record_signature(&store, slot, 0, sig0);
        record_signature(&store, slot, 1, sig1);
        // A signature in a neighbouring slot must not leak in.
        record_signature(&store, slot + 1, 0, Signature::from([9u8; 64]));

        let sigs = store.get_transaction_signatures_for_slot(slot).unwrap();
        assert_eq!(sigs, vec![sig0, sig1, sig2]);

        // A slot with no transactions yields an empty vector.
        let empty = store.get_transaction_signatures_for_slot(999).unwrap();
        assert!(empty.is_empty());
    }

    #[test]
    fn test_get_last_persisted_transaction_position() {
        init_logger!();
        let ledger_path = get_tmp_ledger_path_auto_delete!();
        let store = Ledger::open(ledger_path.path()).unwrap();

        // Empty ledger has no position.
        assert_eq!(
            store.get_last_persisted_transaction_position().unwrap(),
            None
        );

        // Finalized slot 10 (with a block header) and its transactions.
        let finalized_slot = 10;
        record_signature(&store, finalized_slot, 0, Signature::from([1u8; 64]));
        record_signature(&store, finalized_slot, 1, Signature::from([2u8; 64]));
        store
            .write_block(LatestBlockInner::new(
                finalized_slot,
                Hash::new_unique(),
                100,
            ))
            .unwrap();

        // In-progress slot 11 has loose transactions but NO block header, so it
        // is ahead of the latest finalized (blockhash) slot.
        let inprogress_slot = 11;
        record_signature(
            &store,
            inprogress_slot,
            0,
            Signature::from([3u8; 64]),
        );
        record_signature(
            &store,
            inprogress_slot,
            1,
            Signature::from([4u8; 64]),
        );
        record_signature(
            &store,
            inprogress_slot,
            2,
            Signature::from([5u8; 64]),
        );

        // The true maximum key is the in-progress slot's last transaction.
        assert_eq!(
            store.get_last_persisted_transaction_position().unwrap(),
            Some((inprogress_slot, 2))
        );
        // Contrast: the block-header-anchored variant stops at the finalized
        // slot and would resume dedup from the wrong position.
        assert_eq!(
            store.get_latest_transaction_position().unwrap(),
            Some((finalized_slot, 1))
        );
    }
}
