use std::{
    collections::HashMap,
    fmt, fs,
    path::{Path, PathBuf},
    sync::{
        Arc, RwLock,
        atomic::{AtomicI64, Ordering},
    },
};

use bincode::deserialize;
use magicblock_metrics::metrics::{
    HistogramTimer, start_ledger_shutdown_timer,
};
use rocksdb::Direction as IteratorDirection;
use solana_clock::{Slot, UnixTimestamp};
use solana_hash::{HASH_BYTES, Hash};
use solana_measure::measure::Measure;
use solana_pubkey::Pubkey;
use solana_signature::Signature;
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
        columns::{self as cf, DIRTY_COUNT},
        db::Database,
        iterator::IteratorMode,
        ledger_column::LedgerColumn,
        meta::PerfSample,
        options::LedgerOptions,
    },
    errors::{LedgerError, LedgerResult},
    metrics::LedgerRpcApiMetrics,
    store::utils::adjust_ulimit_nofile,
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

    lowest_cleanup_slot: RwLock<Slot>,
    rpc_api_metrics: LedgerRpcApiMetrics,
}

impl fmt::Display for Ledger {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Ledger at {:?}", self.ledger_path)
    }
}

impl Ledger {
    const LOWEST_CLEANUP_SLOT_POISONED: &'static str =
        "lowest_cleanup_slot RwLock poisoned.";

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

            lowest_cleanup_slot: RwLock::<Slot>::default(),
            rpc_api_metrics: LedgerRpcApiMetrics::default(),
        };
        ledger.initialize_lowest_cleanup_slot()?;

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
    // Locking Lowest Cleanup Slot
    // -----------------

    /// Acquires the `lowest_cleanup_slot` lock and returns a tuple of the held lock
    /// and lowest available slot.
    ///
    /// The function will return BlockstoreError::SlotCleanedUp if the input
    /// `slot` has already been cleaned-up.
    fn check_lowest_cleanup_slot(
        &self,
        slot: Slot,
    ) -> LedgerResult<std::sync::RwLockReadGuard<'_, Slot>> {
        // lowest_cleanup_slot is the last slot that was not cleaned up by LedgerCleanupService
        let lowest_cleanup_slot = self
            .lowest_cleanup_slot
            .read()
            .expect(Self::LOWEST_CLEANUP_SLOT_POISONED);
        if *lowest_cleanup_slot > 0 && *lowest_cleanup_slot >= slot {
            return Err(LedgerError::SlotCleanedUp);
        }
        // Make caller hold this lock properly; otherwise LedgerCleanupService can purge/compact
        // needed slots here at any given moment
        Ok(lowest_cleanup_slot)
    }

    /// Acquires the lock of `lowest_cleanup_slot` and returns the tuple of
    /// the held lock and the lowest available slot.
    ///
    /// This function ensures a consistent result by using lowest_cleanup_slot
    /// as the lower bound for reading columns that do not employ strong read
    /// consistency with slot-based delete_range.
    fn ensure_lowest_cleanup_slot(
        &self,
    ) -> (std::sync::RwLockReadGuard<'_, Slot>, Slot) {
        let lowest_cleanup_slot = self
            .lowest_cleanup_slot
            .read()
            .expect(Self::LOWEST_CLEANUP_SLOT_POISONED);
        let lowest_available_slot = (*lowest_cleanup_slot)
            .checked_add(1)
            .expect("overflow from trusted value");

        // Make caller hold this lock properly; otherwise LedgerCleanupService can purge/compact
        // needed slots here at any given moment.
        // Blockstore callers, like rpc, can process concurrent read queries
        (lowest_cleanup_slot, lowest_available_slot)
    }

    /// Returns lowest slot in the ledger if there's any
    pub fn get_lowest_slot(&self) -> Result<Option<Slot>, LedgerError> {
        Ok(self
            .blockhash_cf
            .iter(IteratorMode::Start)?
            .next()
            .map(|(slot, _)| slot))
    }

    pub fn get_lowest_cleanup_slot(&self) -> Slot {
        *self
            .lowest_cleanup_slot
            .read()
            .expect(Self::LOWEST_CLEANUP_SLOT_POISONED)
    }

    /// Initializes lowest slot to cleanup from
    pub fn initialize_lowest_cleanup_slot(&self) -> Result<(), LedgerError> {
        let lowest_cleanup_slot = match self.get_lowest_slot()? {
            Some(lowest_slot) if lowest_slot > 0 => lowest_slot - 1,
            _ => 0,
        };

        info!(
            slot = lowest_cleanup_slot,
            "Initializing lowest cleanup slot"
        );
        self.set_lowest_cleanup_slot(lowest_cleanup_slot);

        Ok(())
    }

    // -----------------
    // Block time
    // -----------------

    pub(crate) fn get_block_time(
        &self,
        slot: Slot,
    ) -> LedgerResult<Option<UnixTimestamp>> {
        let _lock = self.check_lowest_cleanup_slot(slot)?;
        self.blocktime_cf.get(slot)
    }

    pub fn count_block_times(&self) -> LedgerResult<i64> {
        self.blocktime_cf.count_column_using_cache()
    }

    // -----------------
    // Blockhash
    // -----------------

    fn get_block_hash(&self, slot: Slot) -> LedgerResult<Option<Hash>> {
        let _lock = self.check_lowest_cleanup_slot(slot)?;
        self.blockhash_cf.get(slot)
    }

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
    pub fn get_block(
        &self,
        slot: Slot,
    ) -> LedgerResult<Option<VersionedConfirmedBlock>> {
        let blockhash = self.get_block_hash(slot)?;
        let block_time = self.get_block_time(slot)?;

        if block_time.is_none() || blockhash.is_none() {
            return Ok(None);
        }

        let previous_slot = slot.saturating_sub(1);
        let previous_blockhash = self.get_block_hash(previous_slot)?;

        let transactions = {
            let _lock = self.check_lowest_cleanup_slot(slot)?;
            let index_iterator = self
                .slot_signatures_cf
                .iter_current_index_filtered(IteratorMode::From(
                    (slot, u32::MAX),
                    IteratorDirection::Reverse,
                ));

            let mut signatures = vec![];
            for ((tx_slot, _tx_idx), tx_signature) in index_iterator {
                if tx_slot != slot {
                    break;
                }
                signatures.push(Signature::try_from(&*tx_signature)?);
            }

            signatures
                .into_iter()
                .map(|tx_signature| {
                    let transaction = self
                        .read_transaction((tx_signature, slot))?
                        .ok_or(LedgerError::TransactionNotFound)?;
                    let meta = self
                        .transaction_status_cf
                        .get_protobuf((tx_signature, slot))?
                        .ok_or(LedgerError::TransactionStatusMetaNotFound)?;
                    let meta = TransactionStatusMeta::try_from(meta).map_err(
                        |e| {
                            LedgerError::TransactionConversionError(format!(
                                "failed to convert transaction status meta at slot {}: {}",
                                slot, e
                            ))
                        },
                    )?;
                    Ok(VersionedTransactionWithStatusMeta {
                        transaction,
                        meta,
                    })
                })
                .collect::<LedgerResult<Vec<_>>>()
        }?;

        let block_height = Some(slot);
        let block = VersionedConfirmedBlock {
            previous_blockhash: previous_blockhash
                .unwrap_or_default()
                .to_string(),
            blockhash: blockhash.unwrap_or_default().to_string(),

            parent_slot: previous_slot,
            transactions,

            rewards: vec![], // This validator doesn't do voting

            block_time,
            block_height,
            num_partitions: None,
        };

        Ok(Some(block))
    }

    pub fn count_slot_signatures(&self) -> LedgerResult<i64> {
        self.slot_signatures_cf.count_column_using_cache()
    }

    // -----------------
    // Signatures
    // -----------------

    /// Gets all signatures for a given address within the range described by
    /// the provided args.
    ///
    /// * `highest_slot` - Highest slot to consider for the search inclusive.
    ///   Any signatures with a slot higher than this will be ignored.
    ///   In the original implementation this allows ignoring signatures
    ///   that haven't reached a specific commitment level yet.
    ///   For us it will be the current slot in most cases.
    ///   The slot determined for `before` overrides this when provided
    /// - *`upper_limit_signature`* - start searching backwards from this transaction
    ///   signature. If not provided the search starts from the top of the highest_slot
    /// - *`lower_limit_signature`* - search backwards until this transaction signature,
    ///   if found before limit is reached
    /// - *`limit`* -  maximum number of signatures to return (max: 1000)
    ///
    /// ## Example
    ///
    /// Specifying the following:
    ///
    ///  ```text
    ///  pubkey: "<my address>"
    ///  highest_slot: 0
    ///  upper_limit_signature: Some(<sig_upper>)
    ///  lower_limit_signature: Some(<sig_lower>)
    ///  limit: 100
    /// ```
    ///
    /// will find up to 100 signatures that are between upper and lower limit signatures
    /// in this order which is from most recent to oldest:
    ///
    /// ```text
    /// [
    ///   <sigs in same slot as upper_limit_signature with lower transaction index>,
    ///   <sigs with slot_lower_limit < slot < slot_upper_limit>
    ///   <sigs in same slot as lower_limit_signature with higher transaction index>
    /// ]
    /// ```
    ///
    pub fn get_confirmed_signatures_for_address(
        &self,
        pubkey: Pubkey,
        highest_slot: Slot, // highest_confirmed_slot
        upper_limit_signature: Option<Signature>,
        lower_limit_signature: Option<Signature>,
        limit: usize,
    ) -> LedgerResult<SignatureInfosForAddress> {
        self.rpc_api_metrics
            .num_get_confirmed_signatures_for_address
            .fetch_add(1, Ordering::Relaxed);

        // Original implementation uses a more complex ancestor iterator
        // since here we could have missing slots and slots on different forks.
        // That then results in confirmed_unrooted_slots, however we don't have to
        // deal with that since we don't have forks and simple consecutive slots

        // We also changed the approach to filter out the transactions we want
        // (in between upper and lower limit)
        // We do this in the following steps assuming we have upper and lower limits:

        // 1. Determine upper limits
        //
        // newest_slot: the slot where we should start searching downwards from inclusive
        // upper_slot: is the slot from which we should include transactions with lower
        //             tx_index than the upper_limit_signature
        let (found_upper, include_upper, newest_slot, upper_slot) =
            match upper_limit_signature {
                Some(sig) => {
                    let res = self.get_transaction_status(sig, u64::MAX)?;
                    match res {
                        Some((slot, _meta)) => {
                            // Ignore all transactions that happened at the same, or higher slot as the signature
                            let start = slot.saturating_sub(1);
                            // 1. Upper limit slot > highest slot -> don't include it
                            // 2. Upper limit slot <= highest slot  -> include it
                            let include_slot = slot <= highest_slot;

                            // Ensure we respect the highest_slot start limit as well
                            let start = start.min(highest_slot);
                            (true, include_slot, start, slot)
                        }
                        None => (false, false, highest_slot, 0),
                    }
                }
                None => (false, false, highest_slot, 0),
            };

        // 2. Determine lower limits
        //
        // oldest_slot: the slot where we should stop searching downwards inclusive
        // lower_slot: is the slot from which we should include transactions with higher
        //             tx_index than the lower_limit_signature
        let (found_lower, include_lower, lower_slot) =
            match lower_limit_signature {
                Some(sig) => {
                    let res = self.get_transaction_status(sig, u64::MAX)?;
                    // let res = self.get_transaction_status(sig, highest_slot)?;
                    match res {
                        Some((slot, _meta)) => {
                            // 1. Lower limit slot > highest slot -> don't include it
                            // 2. Lower limit slot <= highest slot  -> include it
                            let include_slot = slot <= highest_slot;
                            (true, include_slot, slot)
                        }
                        None => (false, false, 0),
                    }
                }
                None => (false, false, 0),
            };
        #[cfg(test)]
        debug!(
            "lower: {:?}, upper: {:?} (found, include, (newest slot), slot)",
            (found_lower, include_lower, lower_slot),
            (found_upper, include_upper, newest_slot, upper_slot),
        );

        // 3. Find all matching (slot, signature) pairs sorted newest to oldest
        let matching = {
            let mut matching = Vec::new();
            let (_lock, lowest_available_slot) =
                self.ensure_lowest_cleanup_slot();

            // The newest signatures are inside the slot that contains the upper
            // limit signature if it was provided.
            // We include the ones with lower tx_index than that signature
            // (if any for that account).
            if found_upper
                && include_upper
                && upper_slot >= lowest_available_slot
            {
                // SAFETY: found_upper cannot be true if this is None
                let upper_signature = upper_limit_signature.unwrap();

                let index_iterator = self
                    .slot_signatures_cf
                    .iter_current_index_filtered(IteratorMode::From(
                        (upper_slot, u32::MAX),
                        IteratorDirection::Reverse,
                    ));
                let mut transaction_index = None;
                for ((tx_slot, tx_idx), tx_signature) in index_iterator {
                    if tx_slot != upper_slot {
                        break;
                    }

                    let tx_signature = Signature::try_from(&*tx_signature)?;
                    if tx_signature == upper_signature {
                        transaction_index.replace(tx_idx);
                        break;
                    }
                }
                if let Some(index) = transaction_index {
                    let index_iterator = self
                        .address_signatures_cf
                        .iter_current_index_filtered(IteratorMode::From(
                            // The reverse range is not inclusive of the start_slot itself it seems
                            (pubkey, upper_slot, index, upper_signature),
                            IteratorDirection::Reverse,
                        ));

                    for ((address, tx_slot, _tx_idx, signature), _) in
                        index_iterator
                    {
                        if signature == upper_signature {
                            continue;
                        }
                        // Bail out if we reached the max number of signatures to collect
                        if matching.len() >= limit {
                            break;
                        }

                        // Bail out if we reached the iterator space that doesn't match the address
                        if address != pubkey {
                            break;
                        }

                        // Bail out once we reached the lower end of the upper range
                        if tx_slot != upper_slot {
                            break;
                        }

                        matching.push((tx_slot, signature));
                    }
                }
            }

            // Next we add the signatures that are above the slot with the lowest signature
            // and below the slot with the highest signature.
            // If upper limit signature was not provided then the upper slot is the highest_slot
            // If lower limit signature was not provided then we search until we found enough
            // signatures to match the `limit` or run out of signatures entirely.

            // Don't run this if the upper/lower limits already cover all slots
            if newest_slot > lower_slot {
                #[cfg(test)]
                debug!(
                    "Reverse searching ({}, {} -> {})",
                    pubkey, newest_slot, 0,
                );
                let index_iterator = self
                    .address_signatures_cf
                    .iter_current_index_filtered(IteratorMode::From(
                        // The reverse range is not inclusive of the start_slot itself it seems
                        (pubkey, newest_slot, u32::MAX, Signature::default()),
                        IteratorDirection::Reverse,
                    ));

                for ((address, tx_slot, _tx_idx, signature), _) in
                    index_iterator
                {
                    if tx_slot < lowest_available_slot {
                        break;
                    }
                    // Bail out if we reached the max number of signatures to collect
                    if matching.len() >= limit {
                        break;
                    }

                    // Bail out if we reached the iterator space that doesn't match the address
                    if address != pubkey {
                        break;
                    }

                    // Bail out once we reached the lower end of the range for matching addresses
                    if tx_slot <= lower_slot {
                        break;
                    }

                    // The below only happens once we leave the range of our pubkey
                    if tx_slot > newest_slot {
                        #[cfg(test)]
                        debug!(
                            "! signature: {}, slot: {} > {}, address: {}",
                            crate::store::utils::short_signature(&signature),
                            tx_slot,
                            newest_slot,
                            address
                        );
                        continue;
                    }

                    #[cfg(test)]
                    debug!(
                        "in between - signature: {}, slot: {} > {}, address: {}",
                        crate::store::utils::short_signature(&signature),
                        tx_slot,
                        newest_slot,
                        address
                    );
                    matching.push((tx_slot, signature));
                }
            }

            // The oldest signatures are inside the slot that contains the lower
            // limit signature if it was provided
            if found_lower
                && include_lower
                && lower_slot >= lowest_available_slot
            {
                // SAFETY: found_lower cannot be true if this is None
                let lower_signature = lower_limit_signature.unwrap();

                let index_iterator = self
                    .slot_signatures_cf
                    .iter_current_index_filtered(IteratorMode::From(
                        (lower_slot, u32::MAX),
                        IteratorDirection::Reverse,
                    ));
                let mut transaction_index = None;
                for ((tx_slot, tx_idx), tx_signature) in index_iterator {
                    if tx_slot != lower_slot {
                        break;
                    }

                    let tx_signature = Signature::try_from(&*tx_signature)?;
                    if tx_signature == lower_signature {
                        transaction_index.replace(tx_idx);
                        break;
                    }
                }
                if let Some(index) = transaction_index {
                    let index_iterator = self
                        .address_signatures_cf
                        .iter_current_index_filtered(IteratorMode::From(
                            (
                                pubkey,
                                lower_slot,
                                u32::MAX,
                                Signature::default(),
                            ),
                            IteratorDirection::Reverse,
                        ));
                    for ((address, tx_slot, tx_idx, signature), _) in
                        index_iterator
                    {
                        if tx_slot < lowest_available_slot {
                            break;
                        }
                        // Bail out if we reached the max number of signatures to collect
                        if matching.len() >= limit {
                            break;
                        }
                        if tx_slot < lower_slot {
                            break;
                        }

                        // Bail out if we reached the iterator space that doesn't match the address
                        if address != pubkey {
                            break;
                        }
                        if tx_idx <= index {
                            break;
                        }

                        matching.push((tx_slot, signature));
                    }
                }
            }

            matching
        };

        // 4. Resolve blocktimes for each slot we found signatures for
        let mut blocktimes = HashMap::<Slot, UnixTimestamp>::new();
        for (slot, _signature) in &matching {
            if blocktimes.contains_key(slot) {
                continue;
            }
            if let Some(blocktime) = self.get_block_time(*slot)? {
                blocktimes.insert(*slot, blocktime);
            }
        }

        // 5. Build proper Status Infos from and return them
        let mut infos = Vec::<ConfirmedTransactionStatusWithSignature>::new();
        for (slot, signature) in matching {
            let status = self
                .read_transaction_status((signature, slot))?
                .and_then(|x| x.status.err());
            let memo = self.read_transaction_memos(signature, slot)?;
            let block_time = blocktimes.get(&slot).cloned();
            let info = ConfirmedTransactionStatusWithSignature {
                slot,
                signature,
                block_time,
                err: status,
                memo,
                index: 0,
            };
            infos.push(info)
        }

        Ok(SignatureInfosForAddress {
            infos,
            found_upper,
            found_lower,
        })
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
        match self
            .get_confirmed_transaction(signature, highest_confirmed_slot)?
        {
            Some((slot, transaction, meta)) => {
                let block_time = self.get_block_time(slot)?;
                let tx_with_meta = match (transaction, meta) {
                    (Some(transaction), Some(meta)) => {
                        TransactionWithStatusMeta::Complete(
                            VersionedTransactionWithStatusMeta {
                                transaction,
                                meta,
                            },
                        )
                    }
                    (Some(transaction), None) => {
                        let legacy_tx = transaction
                            .into_legacy_transaction()
                            .ok_or_else(|| {
                                LedgerError::TransactionConversionError(
                                    "failed to convert versioned transaction to legacy: \
                                     transaction is v0 (requires metadata)"
                                        .to_string(),
                                )
                            })?;
                        TransactionWithStatusMeta::MissingMetadata(legacy_tx)
                    }
                    (None, Some(_)) | (None, None) => {
                        return Ok(None);
                    }
                };
                Ok(Some(ConfirmedTransactionWithStatusMeta {
                    slot,
                    block_time,
                    tx_with_meta,
                    index: 0,
                }))
            }
            None => Ok(None),
        }
    }

    /// Returns a confirmed transaction and the slot at which it was confirmed
    #[allow(clippy::type_complexity)]
    fn get_confirmed_transaction(
        &self,
        signature: Signature,
        highest_confirmed_slot: Slot,
    ) -> LedgerResult<
        Option<(
            Slot,
            Option<VersionedTransaction>,
            Option<TransactionStatusMeta>,
        )>,
    > {
        self.rpc_api_metrics
            .num_get_complete_transaction
            .fetch_add(1, Ordering::Relaxed);

        let slot_and_meta =
            self.get_transaction_status(signature, highest_confirmed_slot)?;

        let (slot, transaction, meta) = match slot_and_meta {
            Some((slot, meta)) => {
                let transaction = self.read_transaction((signature, slot))?;
                match transaction {
                    Some(transaction) => (slot, Some(transaction), Some(meta)),
                    None => (slot, None, Some(meta)),
                }
            }
            None => {
                let mut iterator = self
                    .transaction_cf
                    .iter_current_index_filtered(IteratorMode::From(
                        (signature, highest_confirmed_slot),
                        IteratorDirection::Forward,
                    ));
                match iterator.next() {
                    Some(((tx_signature, slot), _data))
                        if slot <= highest_confirmed_slot
                            && tx_signature == signature =>
                    {
                        let transaction =
                            self.read_transaction((tx_signature, slot))?;
                        match transaction {
                            Some(tx) => (slot, Some(tx), None),
                            None => return Ok(None),
                        }
                    }
                    Some(_) | None => {
                        // We found neither a transaction nor its status
                        return Ok(None);
                    }
                }
            }
        };

        Ok(Some((slot, transaction, meta)))
    }

    pub fn read_transaction(
        &self,
        index: (Signature, Slot),
    ) -> LedgerResult<Option<VersionedTransaction>> {
        let result = {
            let (_lock, _) = self.ensure_lowest_cleanup_slot();
            self.transaction_cf.get_bytes(index)
        }?;
        match result {
            Some(bytes) => {
                let tx: VersionedTransaction = deserialize(&bytes)?;
                Ok(Some(tx))
            }
            None => Ok(None),
        }
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
        let slot = match self.get_transaction_status(*signature, u64::MAX)? {
            Some((slot, _meta)) => slot,
            None => return Ok(None),
        };

        let transaction = match self.read_transaction((*signature, slot))? {
            Some(tx) => tx,
            None => return Ok(None),
        };

        let is_valid = transaction.verify_and_hash_message().is_ok();
        Ok(Some(is_valid))
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
        let memos = self.transaction_memos_cf.get((signature, slot))?;
        Ok(memos)
    }

    pub fn count_transaction_memos(&self) -> LedgerResult<i64> {
        self.transaction_memos_cf.count_column_using_cache()
    }

    // -----------------
    // TransactionStatus
    // -----------------
    /// Returns a transaction status
    /// * `signature` - Signature of the transaction
    /// * `min_slot` - Lowest slot to consider for the search, i.e. the transaction
    ///   status was added at or before this slot (same as minContextSlot)
    pub fn get_transaction_status(
        &self,
        signature: Signature,
        min_slot: Slot,
    ) -> LedgerResult<Option<(Slot, TransactionStatusMeta)>> {
        let result = {
            let (_lock, lowest_available_slot) =
                self.ensure_lowest_cleanup_slot();
            self.rpc_api_metrics
                .num_get_transaction_status
                .fetch_add(1, Ordering::Relaxed);

            let iterator = self
                .transaction_status_cf
                .iter_current_index_filtered(IteratorMode::From(
                    (signature, lowest_available_slot),
                    IteratorDirection::Forward,
                ));

            let mut result = None;
            for ((stat_signature, slot), _) in iterator {
                if stat_signature == signature && slot <= min_slot {
                    result = self
                        .transaction_status_cf
                        .get_protobuf((signature, slot))?
                        .map(|status| {
                            let status = status.try_into().unwrap();
                            (slot, status)
                        });
                    break;
                }
                // Left the range of the signature we're looking for
                if stat_signature != signature {
                    break;
                }
            }
            result
        };
        Ok(result)
    }

    pub fn read_transaction_status(
        &self,
        index: (Signature, Slot),
    ) -> LedgerResult<Option<TransactionStatusMeta>> {
        let result = {
            let (_lock, _) = self.ensure_lowest_cleanup_slot();
            self.transaction_status_cf.get_protobuf(index)
        }?;
        Ok(result.and_then(|meta| meta.try_into().ok()))
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
        let (_lock, _) = self.ensure_lowest_cleanup_slot();
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
                let include = status.err.is_none() == success;
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

    pub fn count_perf_samples(&self) -> LedgerResult<i64> {
        self.perf_samples_cf.count_column_using_cache()
    }

    pub fn read_slot_signature(
        &self,
        index: (Slot, u32),
    ) -> LedgerResult<Option<Signature>> {
        self.slot_signatures_cf.get(index)
    }

    /// Updates both lowest_cleanup_slot and oldest_slot for CompactionFilter
    /// All slots less or equal to argument will be removed during compaction
    fn set_lowest_cleanup_slot(&self, slot: Slot) {
        let mut lowest_cleanup_slot = self
            .lowest_cleanup_slot
            .write()
            .expect(Self::LOWEST_CLEANUP_SLOT_POISONED);

        let new_lowest_cleanup_slot = std::cmp::max(*lowest_cleanup_slot, slot);
        *lowest_cleanup_slot = new_lowest_cleanup_slot;

        if new_lowest_cleanup_slot == 0 {
            // fresh db case
            self.db.set_oldest_slot(new_lowest_cleanup_slot);
        } else {
            self.db.set_oldest_slot(new_lowest_cleanup_slot + 1);
        }
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
}

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
