use std::{
    collections::HashMap,
    fmt, fs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicI64, Ordering},
        Arc, RwLock,
    },
};

use bincode::{deserialize, serialize};
use log::*;
use rocksdb::{Direction as IteratorDirection, FlushOptions};
use solana_measure::measure::Measure;
use solana_sdk::{
    clock::{Slot, UnixTimestamp},
    hash::{Hash, HASH_BYTES},
    pubkey::Pubkey,
    signature::Signature,
    transaction::{SanitizedTransaction, VersionedTransaction},
};
use solana_storage_proto::convert::generated::{self, ConfirmedTransaction};
use solana_transaction_status::{
    ConfirmedTransactionStatusWithSignature,
    ConfirmedTransactionWithStatusMeta, TransactionStatusMeta,
    VersionedConfirmedBlock, VersionedTransactionWithStatusMeta,
};

use crate::{
    conversions::transaction,
    database::{
        columns as cf,
        columns::{Column, ColumnName, DIRTY_COUNT},
        db::Database,
        iterator::IteratorMode,
        ledger_column::{try_increase_entry_counter, LedgerColumn},
        meta::{AccountModData, AddressSignatureMeta, PerfSample},
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
    account_mod_datas_cf: LedgerColumn<cf::AccountModDatas>,

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
        info!("Opening ledger at {:?}", ledger_path);
        let db = Database::open(&ledger_path, options)?;

        let transaction_status_cf = db.column();
        let address_signatures_cf = db.column();
        let slot_signatures_cf = db.column();
        let blocktime_cf = db.column();
        let blockhash_cf = db.column();
        let transaction_cf = db.column();
        let transaction_memos_cf = db.column();
        let perf_samples_cf = db.column();

        let account_mod_datas_cf = db.column();

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
            account_mod_datas_cf,

            transaction_successful_status_count: AtomicI64::new(DIRTY_COUNT),
            transaction_failed_status_count: AtomicI64::new(DIRTY_COUNT),

            lowest_cleanup_slot: RwLock::<Slot>::default(),
            rpc_api_metrics: LedgerRpcApiMetrics::default(),
        };

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
        self.account_mod_datas_cf.submit_rocksdb_cf_metrics();
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
    ) -> LedgerResult<std::sync::RwLockReadGuard<Slot>> {
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
    ) -> (std::sync::RwLockReadGuard<Slot>, Slot) {
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

        info!("initializing lowest cleanup slot: {}", lowest_cleanup_slot);
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

    // -----------------
    // Block
    // -----------------

    // NOTE: we kept the term block time even tough we don't produce blocks.
    // As far as we are concerned these are just the time when we advanced to
    // a specific slot.
    pub fn write_block(
        &self,
        slot: Slot,
        timestamp: UnixTimestamp,
        blockhash: Hash,
    ) -> LedgerResult<()> {
        self.blocktime_cf.put(slot, &timestamp)?;
        self.blocktime_cf.try_increase_entry_counter(1);

        self.blockhash_cf.put(slot, &blockhash)?;
        self.blockhash_cf.try_increase_entry_counter(1);
        Ok(())
    }

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
            let _lock = self.check_lowest_cleanup_slot(slot);
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
                        .transaction_cf
                        .get_protobuf((tx_signature, slot))?
                        .map(VersionedTransaction::from)
                        .ok_or(LedgerError::TransactionNotFound)?;
                    let meta = self
                        .transaction_status_cf
                        .get_protobuf((tx_signature, slot))?
                        .ok_or(LedgerError::TransactionStatusMetaNotFound)?;
                    Ok(VersionedTransactionWithStatusMeta {
                        transaction,
                        meta: TransactionStatusMeta::try_from(meta).unwrap(),
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
    ///                    Any signatures with a slot higher than this will be ignored.
    ///                    In the original implementation this allows ignoring signatures
    ///                    that haven't reached a specific commitment level yet.
    ///                    For us it will be the current slot in most cases.
    ///                    The slot determined for `before` overrides this when provided
    /// - *`upper_limit_signature`* - start searching backwards from this transaction
    ///     signature. If not provided the search starts from the top of the highest_slot
    /// - *`lower_limit_signature`* - search backwards until this transaction signature,
    ///     if found before limit is reached
    /// - *`limit`* -  maximum number of signatures to return (max: 1000)
    ///
    /// ## Example
    ///
    /// Specifying the following:
    ///
    ///  ```rust
    ///  let pubkey = "<my address>";
    ///  let highest_slot = 0;
    ///  let upper_limit_signature = Some(sig_upper);;
    ///  let lower_limit_signature = Some(sig_lower);
    ///  let limit = 100;
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
        let (found_lower, include_lower, oldest_slot, lower_slot) =
            match lower_limit_signature {
                Some(sig) => {
                    let res = self.get_transaction_status(sig, u64::MAX)?;
                    // let res = self.get_transaction_status(sig, highest_slot)?;
                    match res {
                        Some((slot, _meta)) => {
                            // Ignore all transactions that happened at the same, or lower slot as the signature
                            let end = slot.saturating_add(1);

                            // 1. Lower limit slot > highest slot -> don't include it
                            // 2. Lower limit slot <= highest slot  -> include it
                            let include_slot = slot <= highest_slot;
                            (true, include_slot, end, slot)
                        }
                        None => (false, false, 0, 0),
                    }
                }
                None => (false, false, 0, 0),
            };
        #[cfg(test)]
        debug!(
            "lower: {:?}, upper: {:?} (found, include, newest/oldest slot, slot)",
            (found_upper, include_upper, newest_slot, upper_slot),
            (found_lower, include_lower, oldest_slot, lower_slot)
        );

        // 3. Find all matching (slot, signature) pairs sorted newest to oldest
        let matching = {
            let mut matching = Vec::new();
            let (_lock, _) = self.ensure_lowest_cleanup_slot();

            // The newest signatures are inside the slot that contains the upper
            // limit signature if it was provided.
            // We include the ones with lower tx_index than that signature.
            let mut passed_signature = false;
            if found_upper && include_upper {
                // SAFETY: found_upper cannot be true if this is None
                let upper_signature = upper_limit_signature.unwrap();

                let index_iterator = self
                    .slot_signatures_cf
                    .iter_current_index_filtered(IteratorMode::From(
                        (upper_slot, u32::MAX),
                        IteratorDirection::Reverse,
                    ));
                for ((tx_slot, _tx_idx), tx_signature) in index_iterator {
                    // Bail out if we reached the max number of signatures to collect
                    if matching.len() >= limit {
                        break;
                    }
                    if tx_slot != upper_slot {
                        break;
                    }

                    let tx_signature = Signature::try_from(&*tx_signature)?;
                    if tx_signature == upper_signature {
                        passed_signature = true;
                        continue;
                    }

                    if passed_signature {
                        #[cfg(test)]
                        debug!(
                            "upper - signature: {}, slot: {}+{}",
                            crate::store::utils::short_signature(&tx_signature),
                            tx_slot,
                            _tx_idx,
                        );
                        matching.push((tx_slot, tx_signature));
                    }
                }
            }

            // Next we add the signatures that are above the slot with the lowest signature
            // and below the slot with the highest signature.
            // If upper limit signature was not provided then the upper slot is the highest_slot
            // If lower limit signature was not provided then we search until we found enough
            // signatures to match the `limit` or run out of signatures entirely.

            // Don't run this if the upper/lower limits already cover all slots
            if newest_slot >= oldest_slot {
                #[cfg(test)]
                debug!(
                    "Reverse searching ({}, {} -> {}, {})",
                    pubkey, newest_slot, oldest_slot, 0,
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
                    // Bail out if we reached the max number of signatures to collect
                    if matching.len() >= limit {
                        break;
                    }

                    // Bail out if we reached the iterator space that doesn't match the address
                    if address != pubkey {
                        break;
                    }

                    // Bail out once we reached the lower end of the range for matching addresses
                    if tx_slot < oldest_slot {
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
            if found_lower && include_lower {
                // SAFETY: found_lower cannot be true if this is None
                let lower_signature = lower_limit_signature.unwrap();

                let index_iterator = self
                    .slot_signatures_cf
                    .iter_current_index_filtered(IteratorMode::From(
                        (lower_slot, u32::MAX),
                        IteratorDirection::Reverse,
                    ));
                for ((tx_slot, tx_idx), tx_signature) in index_iterator {
                    // Bail out if we reached the max number of signatures to collect
                    if matching.len() >= limit {
                        break;
                    }
                    if tx_slot != lower_slot {
                        break;
                    }

                    let tx_signature = Signature::try_from(&*tx_signature)?;
                    if tx_signature == lower_signature {
                        break;
                    }

                    debug!(
                        "lower - signature: {}, slot: {}+{}",
                        crate::store::utils::short_signature(&tx_signature),
                        tx_slot,
                        tx_idx,
                    );
                    matching.push((tx_slot, tx_signature));
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
            Some((slot, tx)) => {
                let block_time = self.get_block_time(slot)?;
                let tx = transaction::from_generated_confirmed_transaction(
                    slot, tx, block_time,
                );
                Ok(Some(tx))
            }
            None => Ok(None),
        }
    }

    /// Returns a confirmed transaction and the slot at which it was confirmed
    fn get_confirmed_transaction(
        &self,
        signature: Signature,
        highest_confirmed_slot: Slot,
    ) -> LedgerResult<Option<(Slot, ConfirmedTransaction)>> {
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
                let mut iterator = self.iter_transaction_statuses(
                    Some((signature, highest_confirmed_slot)),
                    false,
                );
                match iterator.next() {
                    Some(Ok((slot, tx_signature, _data))) => {
                        if slot <= highest_confirmed_slot
                            && tx_signature == signature
                        {
                            let slot_and_tx = self
                                .transaction_cf
                                .get_protobuf((tx_signature, slot))?
                                .map(|tx| (slot, tx));
                            if let Some((slot, tx)) = slot_and_tx {
                                (slot, Some(tx), None)
                            } else {
                                // We have a slot, but couldn't resolve a proper transaction
                                return Ok(None);
                            }
                        } else {
                            return Ok(None);
                        }
                    }
                    _ => {
                        // We found neither a transaction nor its status
                        return Ok(None);
                    }
                }
            }
        };

        Ok(Some((
            slot,
            ConfirmedTransaction {
                transaction,
                meta: meta.map(|x| x.into()),
            },
        )))
    }

    /// Writes a confirmed transaction pieced together from the provided inputs
    /// * `signature` - Signature of the transaction
    /// * `slot` - Slot at which the transaction was confirmed
    /// * `transaction` - Transaction to be written, we take a SanititizedTransaction here
    ///                   since that is what we provide Geyser as well
    /// * `status` - status of the transaction
    pub fn write_transaction(
        &self,
        signature: Signature,
        slot: Slot,
        transaction: SanitizedTransaction,
        status: TransactionStatusMeta,
        transaction_slot_index: usize,
    ) -> LedgerResult<()> {
        let tx_account_locks = transaction.get_account_locks_unchecked();

        // 1. Write Transaction Status
        self.write_transaction_status(
            slot,
            signature,
            tx_account_locks.writable,
            tx_account_locks.readonly,
            status,
            transaction_slot_index,
        )?;

        // 2. Write Transaction
        let versioned = transaction.to_versioned_transaction();
        let transaction: generated::Transaction = versioned.into();

        self.transaction_cf
            .put_protobuf((signature, slot), &transaction)?;
        self.transaction_cf.try_increase_entry_counter(1);

        Ok(())
    }

    pub fn read_transaction(
        &self,
        index: (Signature, Slot),
    ) -> LedgerResult<Option<generated::Transaction>> {
        let result = {
            let (_lock, _) = self.ensure_lowest_cleanup_slot();
            self.transaction_cf.get_protobuf(index)
        }?;
        Ok(result)
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

    fn write_transaction_status(
        &self,
        slot: Slot,
        signature: Signature,
        writable_keys: Vec<&Pubkey>,
        readonly_keys: Vec<&Pubkey>,
        status: TransactionStatusMeta,
        transaction_slot_index: usize,
    ) -> LedgerResult<()> {
        let transaction_slot_index = u32::try_from(transaction_slot_index)
            .map_err(|_| LedgerError::TransactionIndexOverflow)?;

        for address in writable_keys {
            self.address_signatures_cf.put(
                (*address, slot, transaction_slot_index, signature),
                &AddressSignatureMeta { writeable: true },
            )?;
            self.address_signatures_cf.try_increase_entry_counter(1);
        }
        for address in readonly_keys {
            self.address_signatures_cf.put(
                (*address, slot, transaction_slot_index, signature),
                &AddressSignatureMeta { writeable: false },
            )?;
            self.address_signatures_cf.try_increase_entry_counter(1);
        }

        self.slot_signatures_cf
            .put((slot, transaction_slot_index), &signature)?;
        self.slot_signatures_cf.try_increase_entry_counter(1);

        let status = status.into();
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
    /// - `from`    - The starting point of the iterator, defaults to [`Signature::default(), 0`]
    /// - `success` - If true, only successful transactions are returned,
    ///               otherwise only failed ones
    pub fn iter_transaction_statuses(
        &self,
        from: Option<(Signature, Slot)>,
        success: bool,
    ) -> impl Iterator<
        Item = LedgerResult<(
            Slot,
            Signature,
            generated::TransactionStatusMeta,
        )>,
    > + '_ {
        let (_lock, _) = self.ensure_lowest_cleanup_slot();
        let from = from.unwrap_or((Signature::default(), 0));
        self.transaction_status_cf
            .iter_protobuf(IteratorMode::From(from, IteratorDirection::Forward))
            .filter_map(move |res| {
                let ((signature, slot), status) = match res {
                    Ok(((signature, slot), status)) => {
                        ((signature, slot), status)
                    }
                    Err(err) => return Some(Err(err)),
                };
                let exclude = status.err.is_some() && success;
                if exclude {
                    None
                } else {
                    Some(Ok((slot, signature, status)))
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
        for res in self.iter_transaction_statuses(None, success) {
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

    // -----------------
    // AccountModDatas
    // -----------------
    pub fn write_account_mod_data(
        &self,
        id: u64,
        data: &AccountModData,
    ) -> LedgerResult<()> {
        self.account_mod_datas_cf.put(id, data)?;
        self.account_mod_datas_cf.try_increase_entry_counter(1);
        Ok(())
    }

    pub fn read_account_mod_data(
        &self,
        id: u64,
    ) -> LedgerResult<Option<AccountModData>> {
        self.account_mod_datas_cf.get(id)
    }

    pub fn count_account_mod_data(&self) -> LedgerResult<i64> {
        self.account_mod_datas_cf.count_column_using_cache()
    }

    pub fn read_slot_signature(
        &self,
        index: (Slot, u32),
    ) -> LedgerResult<Option<Signature>> {
        self.slot_signatures_cf.get(index)
    }

    /// Updates both lowest_cleanup_slot and oldest_slot for CompactionFilter
    /// All slots less or equal to argument will be removed during compaction
    pub fn set_lowest_cleanup_slot(&self, slot: Slot) {
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

    /// Permanently removes ledger data for slots in the inclusive range `[from_slot, to_slot]`.
    /// # Note:
    /// - This is a destructive operation that cannot be undone
    /// - Requires exclusive access to the lowest cleanup slot tracker
    /// - All deletions are atomic (either all succeed or none do)
    pub fn delete_slot_range(
        &self,
        from_slot: Slot,
        to_slot: Slot,
    ) -> LedgerResult<()> {
        self.set_lowest_cleanup_slot(to_slot);

        let mut batch = self.db.batch();
        let num_deleted_slots = to_slot + 1 - from_slot;
        self.blocktime_cf.delete_range_in_batch(
            &mut batch,
            from_slot,
            to_slot + 1,
        );
        self.blockhash_cf.delete_range_in_batch(
            &mut batch,
            from_slot,
            to_slot + 1,
        );
        self.perf_samples_cf.delete_range_in_batch(
            &mut batch,
            from_slot,
            to_slot + 1,
        );

        let mut slot_signatures_deleted = 0;
        let mut transaction_status_deleted = 0;
        let mut transactions_deleted = 0;
        let mut transaction_memos_deleted = 0;
        let mut address_signatures_deleted = 0;
        self.slot_signatures_cf
            .iter(IteratorMode::From(
                (from_slot, u32::MIN),
                IteratorDirection::Forward,
            ))?
            .take_while(|((slot, _), _)| slot <= &to_slot)
            .try_for_each(|((slot, transaction_index), raw_signature)| {
                self.slot_signatures_cf
                    .delete_in_batch(&mut batch, (slot, transaction_index));
                slot_signatures_deleted += 1;

                let signature = Signature::try_from(raw_signature.as_ref())?;
                self.transaction_status_cf
                    .delete_in_batch(&mut batch, (signature, slot));
                transaction_status_deleted += 1;

                self.transaction_cf
                    .delete_in_batch(&mut batch, (signature, slot));
                transactions_deleted += 1;

                self.transaction_memos_cf
                    .delete_in_batch(&mut batch, (signature, slot));
                transaction_memos_deleted += 1;

                let transaction = self
                    .transaction_cf
                    .get_protobuf((signature, slot))?
                    .map(VersionedTransaction::from)
                    .ok_or(LedgerError::TransactionNotFound)?;

                transaction.message.static_account_keys().iter().for_each(
                    |address| {
                        self.address_signatures_cf.delete_in_batch(
                            &mut batch,
                            (*address, slot, transaction_index, signature),
                        );
                        address_signatures_deleted += 1;
                    },
                );

                // TODO(edwin): add AccountModData cleanup
                Ok::<_, LedgerError>(())
            })?;

        self.db.write(batch)?;

        self.blocktime_cf
            .try_decrease_entry_counter(num_deleted_slots);
        self.blockhash_cf
            .try_decrease_entry_counter(num_deleted_slots);
        self.perf_samples_cf
            .try_decrease_entry_counter(num_deleted_slots);
        self.slot_signatures_cf
            .try_decrease_entry_counter(slot_signatures_deleted);
        self.transaction_status_cf
            .try_decrease_entry_counter(transaction_status_deleted);
        self.transaction_cf
            .try_decrease_entry_counter(transactions_deleted);
        self.transaction_memos_cf
            .try_decrease_entry_counter(transaction_memos_deleted);
        self.address_signatures_cf
            .try_decrease_entry_counter(address_signatures_deleted);

        // To not spend time querying DB for value we set drop the counter
        // This shouldn't happen very often due to rarity of actual truncations.
        self.transaction_successful_status_count
            .store(DIRTY_COUNT, Ordering::Release);
        self.transaction_failed_status_count
            .store(DIRTY_COUNT, Ordering::Release);

        Ok(())
    }

    pub fn compact_slot_range_cf<C: Column + ColumnName>(
        &self,
        from: Option<C::Index>,
        to: Option<C::Index>,
    ) {
        self.db.column::<C>().compact_range(from, to);
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
            self.account_mod_datas_cf.handle(),
        ];

        self.db
            .backend
            .flush_cfs_opt(&cfs, &FlushOptions::default())
    }

    /// Graceful db shutdown
    pub fn shutdown(&self, wait: bool) -> LedgerResult<()> {
        self.flush()?;
        self.db.backend.db.cancel_all_background_work(wait);

        Ok(())
    }
}

// -----------------
// Tests
// -----------------
#[cfg(test)]
mod tests {
    use solana_sdk::{
        clock::UnixTimestamp,
        instruction::{CompiledInstruction, InstructionError},
        message::{v0, MessageHeader, SimpleAddressLoader, VersionedMessage},
        pubkey::Pubkey,
        signature::{Keypair, Signature},
        signer::Signer,
        transaction::{TransactionError, VersionedTransaction},
        transaction_context::TransactionReturnData,
    };
    use solana_transaction_status::{
        ConfirmedTransactionWithStatusMeta, InnerInstruction,
        InnerInstructions, TransactionStatusMeta, TransactionWithStatusMeta,
        VersionedTransactionWithStatusMeta,
    };
    use tempfile::{Builder, TempDir};
    use test_tools_core::init_logger;

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
                status: solana_sdk::transaction::Result::<()>::Err(
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
                    error!("VersionedTransaction::try_into failed: {:?}", e)
                })
                .unwrap(),
            Default::default(),
            false,
            SimpleAddressLoader::Enabled(meta.loaded_addresses.clone()),
            &Default::default(),
        )
        .map_err(|e| error!("SanitizedTransaction::try_new failed: {:?}", e))
        .unwrap();

        (
            ConfirmedTransactionWithStatusMeta {
                slot,
                block_time,
                tx_with_meta,
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
                    signature,
                    keys_as_ref!(writable_keys),
                    keys_as_ref!(readonly_keys),
                    meta.clone(),
                    0,
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
                    signature,
                    keys_as_ref!(writable_keys),
                    keys_as_ref!(readonly_keys),
                    meta.clone(),
                    0,
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
                sig_uno,
                keys_as_ref!(writable_keys),
                keys_as_ref!(readonly_keys),
                status_uno.clone(),
                0
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
                sig_dos,
                keys_as_ref!(writable_keys),
                keys_as_ref!(readonly_keys),
                status_dos.clone(),
                0,
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
            .get_confirmed_transaction(sig_uno, 0)
            .unwrap()
            .is_none());
        assert!(store
            .get_confirmed_transaction(sig_dos, 0)
            .unwrap()
            .is_none());

        // 1. Write first transaction and block time for relevant slot
        assert!(store
            .write_transaction(
                sig_uno,
                slot_uno,
                sanitized_uno.clone(),
                tx_uno.tx_with_meta.get_status_meta().unwrap(),
                0,
            )
            .is_ok());
        assert!(store
            .write_block(slot_uno, block_time_uno, block_hash_uno)
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
        assert!(store
            .write_transaction(
                sig_dos,
                slot_dos,
                sanitized_dos.clone(),
                tx_dos.tx_with_meta.get_status_meta().unwrap(),
                0
            )
            .is_ok());
        assert!(store
            .write_block(slot_dos, block_time_dos, block_hash_dos)
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
        let (read_uno, write_uno) = {
            let (meta, writable_keys, readonly_keys) =
                create_transaction_status_meta(5);
            let read_uno = readonly_keys[0];
            let write_uno = writable_keys[0];
            assert!(store
                .write_transaction_status(
                    slot_uno,
                    signature_uno,
                    keys_as_ref!(writable_keys),
                    keys_as_ref!(readonly_keys),
                    meta.clone(),
                    0,
                )
                .is_ok());
            (read_uno, write_uno)
        };

        let (signature_dos, slot_dos) = (Signature::new_unique(), 20);
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
                    signature_dos,
                    keys_as_ref!(writable_keys),
                    keys_as_ref!(readonly_keys),
                    meta.clone(),
                    0,
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
                    signature_dos_2,
                    keys_as_ref!(writable_keys),
                    keys_as_ref!(readonly_keys),
                    meta.clone(),
                    1,
                )
                .is_ok());

            (read_dos, write_dos)
        };

        let (signature_tres, slot_tres) = (Signature::new_unique(), 30);
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
                    signature_tres,
                    keys_as_ref!(writable_keys),
                    keys_as_ref!(readonly_keys),
                    meta.clone(),
                    0,
                )
                .is_ok());
            (read_tres, write_tres)
        };

        let (signature_cuatro, slot_cuatro) = (Signature::new_unique(), 31);
        let (read_cuatro, _write_cuatro) = {
            let (meta, writable_keys, readonly_keys) =
                create_transaction_status_meta(5);
            let read_cuatro = readonly_keys[0];
            let write_cuatro = writable_keys[0];
            assert!(store
                .write_transaction_status(
                    slot_cuatro,
                    signature_cuatro,
                    keys_as_ref!(writable_keys),
                    keys_as_ref!(readonly_keys),
                    meta.clone(),
                    0,
                )
                .is_ok());
            (read_cuatro, write_cuatro)
        };

        let (signature_cinco, slot_cinco) = (Signature::new_unique(), 31);
        let (_read_cinco, _write_cinco) = {
            let (meta, writable_keys, readonly_keys) =
                create_transaction_status_meta(5);
            let read_cinco = readonly_keys[0];
            let write_cinco = writable_keys[0];
            assert!(store
                .write_transaction_status(
                    slot_cinco,
                    signature_cinco,
                    keys_as_ref!(writable_keys),
                    keys_as_ref!(readonly_keys),
                    meta.clone(),
                    0,
                )
                .is_ok());
            (read_cinco, write_cinco)
        };

        let (signature_seis, slot_seis) = (Signature::new_unique(), 32);
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
                    signature_seis,
                    keys_as_ref!(writable_keys),
                    keys_as_ref!(readonly_keys),
                    meta.clone(),
                    0,
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
        assert!(store.write_block(slot_uno, 1, Hash::new_unique()).is_ok());
        assert!(store.write_block(slot_dos, 2, Hash::new_unique()).is_ok());
        assert!(store.write_block(slot_tres, 3, Hash::new_unique()).is_ok());
        assert!(store
            .write_block(slot_cuatro, 4, Hash::new_unique())
            .is_ok());
        assert!(store.write_block(slot_cinco, 5, Hash::new_unique()).is_ok());
        assert!(store.write_block(slot_seis, 6, Hash::new_unique()).is_ok());

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
                vec![
                    (slot_seis, signature_seis),
                    (slot_tres, signature_tres),
                    (slot_dos, signature_dos_2),
                ]
            );
        }
        // Before/Until configured
        {
            let res = store
                .get_confirmed_signatures_for_address(
                    read_uno,
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
                vec![(slot_dos, signature_dos), (slot_uno, signature_uno),]
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
        let mut tx_idx = 0;
        let read_uno = {
            let (meta, writable_keys, readonly_keys) =
                create_transaction_status_meta(5);
            let read_uno = readonly_keys[0];
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
                    tx_idx = 0;
                }
                assert!(store
                    .write_transaction_status(
                        *slot,
                        *signature,
                        keys_as_ref!(writable_keys.clone()),
                        keys_as_ref!(readonly_keys.clone()),
                        meta.clone(),
                        tx_idx
                    )
                    .is_ok());
                tx_idx += 1;
            }

            assert!(store.write_block(slot1, 1, Hash::new_unique()).is_ok());
            assert!(store.write_block(slot2, 2, Hash::new_unique()).is_ok());
            assert!(store.write_block(slot3, 3, Hash::new_unique()).is_ok());
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
            assert!(store
                .write_transaction(
                    sig_uno,
                    slot_uno,
                    sanitized_uno.clone(),
                    tx_uno.tx_with_meta.get_status_meta().unwrap(),
                    0,
                )
                .is_ok());

            assert!(store
                .write_block(slot_uno, 100, Hash::new_unique())
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
            assert!(store
                .write_transaction(
                    sig_dos,
                    slot_dos,
                    sanitized_dos.clone(),
                    tx_dos.tx_with_meta.get_status_meta().unwrap(),
                    0,
                )
                .is_ok());
            assert!(store
                .write_block(slot_dos, 100, Hash::new_unique())
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
    fn test_truncate_slots() {
        init_logger!();
        let ledger_path = get_tmp_ledger_path_auto_delete!();
        let store = Ledger::open(ledger_path.path()).unwrap();

        // Create test data
        let slots_to_delete = [10, 15];
        let slots_to_preserve = [20];
        let test_data: Vec<_> = slots_to_delete
            .iter()
            .chain(slots_to_preserve.iter())
            .map(|&slot| {
                let sig = Signature::new_unique();
                let (tx, sanitized) =
                    create_confirmed_transaction(slot, 5, Some(100), None);
                (sig, slot, tx, sanitized)
            })
            .collect();

        // Write data to ledger
        test_data.iter().for_each(|(sig, slot, tx, sanitized)| {
            store
                .write_transaction(
                    *sig,
                    *slot,
                    sanitized.clone(),
                    tx.tx_with_meta.get_status_meta().unwrap(),
                    0,
                )
                .unwrap();
            store.write_block(*slot, 100, Hash::new_unique()).unwrap();
            store
                .write_transaction_memos(
                    sig,
                    *slot,
                    format!("Memo for slot {}", slot),
                )
                .unwrap();
        });

        // Truncate slots 10-15 (should remove first two entries)
        assert!(store
            .delete_slot_range(
                *slots_to_delete.first().unwrap(),
                *slots_to_delete.last().unwrap()
            )
            .is_ok());

        // Consistency checks
        let (to_delete, to_preserve) =
            test_data.split_at(slots_to_delete.len());
        to_delete.iter().for_each(|(sig, slot, _, _)| {
            assert!(store
                .transaction_cf
                .get_protobuf((*sig, *slot))
                .unwrap()
                .is_none());
            assert!(store
                .transaction_status_cf
                .get_protobuf((*sig, *slot))
                .unwrap()
                .is_none());
            assert!(store.blocktime_cf.get(*slot).unwrap().is_none());
            assert!(store
                .read_transaction_memos(*sig, *slot)
                .unwrap()
                .is_none());
        });
        to_preserve.iter().for_each(|(sig, slot, _, _)| {
            assert!(store
                .transaction_cf
                .get_protobuf((*sig, *slot))
                .unwrap()
                .is_some());
            assert!(store
                .transaction_status_cf
                .get_protobuf((*sig, *slot))
                .unwrap()
                .is_some());
            assert!(store.blocktime_cf.get(*slot).unwrap().is_some());
            assert_eq!(
                store.read_transaction_memos(*sig, *slot).unwrap(),
                Some(format!("Memo for slot {}", slot))
            );
        });
    }
}
