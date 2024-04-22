use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    sync::{atomic::Ordering, Arc, RwLock},
};

use bincode::deserialize;
use log::*;
use rocksdb::Direction as IteratorDirection;
use solana_measure::measure::Measure;
use solana_sdk::{
    clock::{Slot, UnixTimestamp},
    pubkey::Pubkey,
    signature::Signature,
    transaction::{SanitizedTransaction, VersionedTransaction},
};
use solana_storage_proto::convert::generated::{self, ConfirmedTransaction};
use solana_transaction_status::{
    ConfirmedTransactionStatusWithSignature,
    ConfirmedTransactionWithStatusMeta, TransactionStatusMeta,
    TransactionWithStatusMeta, VersionedTransactionWithStatusMeta,
};

use crate::{
    conversions::{self, transaction},
    database::{
        columns as cf,
        db::Database,
        iterator::IteratorMode,
        ledger_column::LedgerColumn,
        meta::{AddressSignatureMeta, TransactionStatusIndexMeta},
        options::LedgerOptions,
    },
    errors::{LedgerError, LedgerResult},
    metrics::LedgerRpcApiMetrics,
    store::utils::adjust_ulimit_nofile,
};

#[derive(Default, Debug)]
pub struct SignatureInfosForAddress {
    pub infos: Vec<ConfirmedTransactionStatusWithSignature>,
    pub found_before: bool,
}

pub struct Store {
    ledger_path: PathBuf,
    db: Arc<Database>,

    transaction_status_cf: LedgerColumn<cf::TransactionStatus>,
    address_signatures_cf: LedgerColumn<cf::AddressSignatures>,
    transaction_status_index_cf: LedgerColumn<cf::TransactionStatusIndex>,
    blocktime_cf: LedgerColumn<cf::Blocktime>,
    transaction_cf: LedgerColumn<cf::Transaction>,

    highest_primary_index_slot: RwLock<Option<Slot>>,

    pub lowest_cleanup_slot: RwLock<Slot>,
    rpc_api_metrics: LedgerRpcApiMetrics,
}

impl Store {
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
    pub fn open(ledger_path: &Path) -> std::result::Result<Self, LedgerError> {
        Self::do_open(ledger_path, LedgerOptions::default())
    }

    pub fn open_with_options(
        ledger_path: &Path,
        options: LedgerOptions,
    ) -> std::result::Result<Self, LedgerError> {
        Self::do_open(ledger_path, options)
    }

    fn do_open(
        ledger_path: &Path,
        options: LedgerOptions,
    ) -> std::result::Result<Self, LedgerError> {
        fs::create_dir_all(ledger_path)?;
        let blockstore_path = ledger_path.join(
            options
                .column_options
                .shred_storage_type
                .blockstore_directory(),
        );
        adjust_ulimit_nofile(options.enforce_ulimit_nofile)?;

        // Open the database
        let mut measure = Measure::start("blockstore open");
        info!("Opening blockstore at {:?}", blockstore_path);
        let db = Database::open(&blockstore_path, options)?;

        let transaction_status_cf = db.column();
        let address_signatures_cf = db.column();
        let transaction_status_index_cf = db.column();
        let blocktime_cf = db.column();
        let transaction_cf = db.column();

        let db = Arc::new(db);

        // NOTE: left out max root

        measure.stop();
        info!("Opening blockstore done; {measure}");

        let blockstore = Store {
            ledger_path: ledger_path.to_path_buf(),
            db,

            transaction_status_cf,
            address_signatures_cf,
            transaction_status_index_cf,
            blocktime_cf,
            transaction_cf,

            highest_primary_index_slot: RwLock::<Option<Slot>>::default(),

            lowest_cleanup_slot: RwLock::<Slot>::default(),
            rpc_api_metrics: LedgerRpcApiMetrics::default(),
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
        self.transaction_status_cf.submit_rocksdb_cf_metrics();
        self.address_signatures_cf.submit_rocksdb_cf_metrics();
        self.transaction_status_index_cf.submit_rocksdb_cf_metrics();
        self.blocktime_cf.submit_rocksdb_cf_metrics();
        self.transaction_cf.submit_rocksdb_cf_metrics();
    }

    // -----------------
    // Utility
    // -----------------
    fn cleanup_old_entries(&self) -> std::result::Result<(), LedgerError> {
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
        // Left out cleanup by "old software" since we won't encounter that
        Ok(())
    }

    fn set_highest_primary_index_slot(&self, slot: Option<Slot>) {
        *self.highest_primary_index_slot.write().unwrap() = slot;
    }

    fn update_highest_primary_index_slot(
        &self,
    ) -> std::result::Result<(), LedgerError> {
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
        let lowest_cleanup_slot = self.lowest_cleanup_slot.read().unwrap();
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
        let lowest_cleanup_slot = self.lowest_cleanup_slot.read().unwrap();
        let lowest_available_slot = (*lowest_cleanup_slot)
            .checked_add(1)
            .expect("overflow from trusted value");

        // Make caller hold this lock properly; otherwise LedgerCleanupService can purge/compact
        // needed slots here at any given moment.
        // Blockstore callers, like rpc, can process concurrent read queries
        (lowest_cleanup_slot, lowest_available_slot)
    }

    // -----------------
    // BlockTime
    // -----------------

    // NOTE: we kept the term block time even tough we don't produce blocks.
    // As far as we are concerned these are just the time when we advanced to
    // a specific slot.
    pub fn cache_block_time(
        &self,
        slot: Slot,
        timestamp: solana_sdk::clock::UnixTimestamp,
    ) -> LedgerResult<()> {
        self.blocktime_cf.put(slot, &timestamp)
    }

    fn get_block_time(
        &self,
        slot: Slot,
    ) -> LedgerResult<Option<solana_sdk::clock::UnixTimestamp>> {
        let _lock = self.check_lowest_cleanup_slot(slot)?;
        self.blocktime_cf.get(slot)
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
    /// - *`before`* - start searching backwards from this transaction signature
    ///                If not provided the search starts from the top of the
    ///                highest_slot
    /// - *`until`* -  search until this transaction signature, if found before
    ///                limit reached
    /// - *`limit`* -  maximum number of signatures to return (max: 1000)
    pub fn get_confirmed_signatures_for_address(
        &self,
        pubkey: Pubkey,
        highest_slot: Slot, // highest_confirmed_slot
        before: Option<Signature>,
        until: Option<Signature>,
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
        // (in between before and until)

        let mut matching = Vec::new();
        let (found_before, start_slot) = match before {
            Some(sig) => {
                let res = self.get_transaction_status(sig, 0)?;
                match res {
                    // TODO: add same slot signaturess for the address that happened before the sig
                    // to matching
                    Some((slot, _meta)) => {
                        // Ignore all transactions that happened at the same, or higher slot as the signature
                        let start = slot.saturating_sub(1);
                        // Ensure we respect the highest_slot start limit as well
                        let start = start.min(highest_slot);
                        (true, start)
                    }
                    None => (false, highest_slot),
                }
            }
            None => (false, highest_slot),
        };
        debug!("Reverse searching ({}, {}, {})", pubkey, start_slot, 0,);

        {
            let (lock, _) = self.ensure_lowest_cleanup_slot();
            let index_iterator = self
                .address_signatures_cf
                .iter_current_index_filtered(IteratorMode::From(
                    // The reverse range is not inclusive of the start_slot itself it seems
                    (pubkey, start_slot + 1, 0, Signature::default()),
                    IteratorDirection::Reverse,
                ))?;

            for (
                (address, transaction_slot, _transaction_index, signature),
                _,
            ) in index_iterator
            {
                // Bail out if we reached the max number of signatures to collect
                if matching.len() >= limit {
                    break;
                }

                // Bail out if we reached the iterator space that doesn't match the address
                if address != pubkey {
                    break;
                }

                if transaction_slot > start_slot {
                    debug!(
                        "! signature: {}, slot: {} > {}, address: {}",
                        crate::store::utils::short_signature(&signature),
                        transaction_slot,
                        start_slot,
                        address
                    );
                    continue;
                }

                matching.push((transaction_slot, signature));

                debug!(
                    "signature: {}, slot: {:?}, address: {}",
                    crate::store::utils::short_signature(&signature),
                    transaction_slot,
                    address
                );
            }
        }

        let mut blocktimes = HashMap::<Slot, UnixTimestamp>::new();
        for (slot, signature) in &matching {
            if blocktimes.contains_key(slot) {
                continue;
            }
            if let Some(blocktime) = self.get_block_time(*slot)? {
                blocktimes.insert(*slot, blocktime);
            }
        }

        let mut infos = Vec::<ConfirmedTransactionStatusWithSignature>::new();
        for (slot, signature) in matching {
            let status = self
                .read_transaction_status((signature, slot))?
                .and_then(|x| x.status.err());
            let block_time = blocktimes.get(&slot).cloned();
            let info = ConfirmedTransactionStatusWithSignature {
                slot,
                signature,
                block_time,
                err: status,
                // TODO: @@@ledger support memos
                memo: None,
            };
            infos.push(info)
        }

        Ok(SignatureInfosForAddress {
            infos,
            found_before,
        })
    }

    // Returns all signatures for an address in a particular slot, regardless of whether that slot
    // has been rooted. The transactions will be ordered by their occurrence in the block
    fn find_address_signatures_for_slot(
        &self,
        pubkey: Pubkey,
        slot: Slot,
    ) -> LedgerResult<Vec<(Slot, Signature)>> {
        let (lock, lowest_available_slot) = self.ensure_lowest_cleanup_slot();
        let mut signatures: Vec<(Slot, Signature)> = vec![];
        if slot < lowest_available_slot {
            return Ok(signatures);
        }

        let index_iterator = self
            .address_signatures_cf
            .iter_current_index_filtered(IteratorMode::From(
                (pubkey, slot, 0, Signature::default()),
                IteratorDirection::Forward,
            ))?;
        for ((address, transaction_slot, _transaction_index, signature), _) in
            index_iterator
        {
            // The iterator starts at exact (pubkey, slot, ..), but will keep iterating and match
            // keys with different pubkey and slot which is why we break once we hit one
            if transaction_slot > slot || address != pubkey {
                break;
            }
            signatures.push((slot, signature));
        }
        drop(lock);
        Ok(signatures)
    }

    fn get_slot_signatures_rev(
        &self,
        slot: Slot,
    ) -> LedgerResult<Vec<Signature>> {
        let index_iterator = self
            .transaction_status_cf
            .iter_current_index_filtered(IteratorMode::From(
                (Signature::default(), slot),
                IteratorDirection::Forward,
            ))?;
        for ((signature, transaction_slot), _) in index_iterator {
            // if transaction_slot > slot {
            //     break;
            // }
            debug!("signature: {:?}, slot: {:?}", signature, transaction_slot);
        }
        Ok(vec![])
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
                let mut iterator = self
                    .transaction_cf
                    .iter_current_index_filtered(IteratorMode::From(
                        (signature, highest_confirmed_slot),
                        IteratorDirection::Forward,
                    ))?;
                match iterator.next() {
                    Some(((signature, slot), _data)) => {
                        let slot_and_tx = self
                            .transaction_cf
                            .get_protobuf((signature, slot))?
                            .map(|tx| (slot, tx));
                        if let Some((slot, tx)) = slot_and_tx {
                            (slot, Some(tx), None)
                        } else {
                            // We have a slot, but couldn't resolve a proper transaction
                            return Ok(None);
                        }
                    }
                    None => {
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
    ///                   since that is what we get provided by Geyser
    /// * `status` - status of the transaction
    pub fn write_transaction(
        &self,
        signature: Signature,
        slot: Slot,
        transaction: SanitizedTransaction,
        status: TransactionStatusMeta,
        transaction_index: usize,
    ) -> LedgerResult<()> {
        // 1. Write Transaction Status
        self.write_transaction_status(
            slot,
            signature,
            status,
            transaction_index,
        );

        // 2. Write Transaction
        let versioned = transaction.to_versioned_transaction();
        let transaction: generated::Transaction = versioned.into();
        self.transaction_cf
            .put_protobuf((signature, slot), &transaction)?;
        Ok(())
    }

    fn read_transaction(
        &self,
        index: (Signature, Slot),
    ) -> LedgerResult<Option<generated::Transaction>> {
        let result = {
            let (lock, lowest_available_slot) =
                self.ensure_lowest_cleanup_slot();
            self.transaction_cf.get_protobuf(index)
        }?;
        Ok(result)
    }

    // -----------------
    // TransactionStatus
    // -----------------
    /// Returns a transaction status
    /// * `signature` - Signature of the transaction
    /// * `min_slot` - Highest slot to consider for the search, i.e. the transaction
    /// status was added at or after this slot (same as minContextSlot)
    pub fn get_transaction_status(
        &self,
        signature: Signature,
        min_slot: Slot,
    ) -> LedgerResult<Option<(Slot, TransactionStatusMeta)>> {
        let result = {
            let (lock, lowest_available_slot) =
                self.ensure_lowest_cleanup_slot();
            self.rpc_api_metrics
                .num_get_transaction_status
                .fetch_add(1, Ordering::Relaxed);

            let mut iterator = self
                .transaction_status_cf
                .iter_current_index_filtered(IteratorMode::From(
                    (signature, min_slot.max(lowest_available_slot)),
                    IteratorDirection::Forward,
                ))?;

            let mut result = None;
            for ((stat_signature, slot), _) in iterator {
                if stat_signature == signature {
                    debug!(
                        "{} == {}",
                        crate::store::utils::short_signature(&stat_signature),
                        crate::store::utils::short_signature(&signature)
                    );
                    result = self
                        .transaction_status_cf
                        .get_protobuf((signature, slot))?
                        .map(|status| {
                            let status = status.try_into().unwrap();
                            (slot, status)
                        });
                    break;
                }
                debug!(
                    "{} != {}",
                    crate::store::utils::short_signature(&stat_signature),
                    crate::store::utils::short_signature(&signature)
                );
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
            let (lock, _) = self.ensure_lowest_cleanup_slot();
            self.transaction_status_cf.get_protobuf(index)
        }?;
        Ok(result.and_then(|meta| meta.try_into().ok()))
    }

    fn write_transaction_status(
        &self,
        slot: Slot,
        signature: Signature,
        status: TransactionStatusMeta,
        transaction_index: usize,
    ) -> LedgerResult<()> {
        let transaction_index = u32::try_from(transaction_index)
            .map_err(|_| LedgerError::TransactionIndexOverflow)?;
        for address in &status.loaded_addresses.writable {
            self.address_signatures_cf.put(
                (*address, slot, transaction_index, signature),
                &AddressSignatureMeta { writeable: true },
            )?;
        }
        for address in &status.loaded_addresses.readonly {
            self.address_signatures_cf.put(
                (*address, slot, transaction_index, signature),
                &AddressSignatureMeta { writeable: false },
            )?;
        }

        let status = status.into();
        self.transaction_status_cf
            .put_protobuf((signature, slot), &status)?;
        Ok(())
    }
}

// -----------------
// Tests
// -----------------
#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use solana_sdk::{
        clock::UnixTimestamp,
        instruction::{CompiledInstruction, InstructionError},
        message::{
            v0::{self, LoadedAddresses},
            MessageHeader, SimpleAddressLoader, VersionedMessage,
        },
        pubkey::Pubkey,
        signature::{Keypair, Signature},
        signer::Signer,
        transaction::TransactionError,
        transaction_context::TransactionReturnData,
    };
    use solana_transaction_status::{
        ConfirmedTransactionWithStatusMeta, InnerInstruction,
        InnerInstructions, TransactionStatusMeta,
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

    fn create_transaction_status_meta(fee: u64) -> TransactionStatusMeta {
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
        let test_loaded_addresses = LoadedAddresses {
            writable: vec![Pubkey::new_unique()],
            readonly: vec![Pubkey::new_unique()],
        };
        let test_return_data = TransactionReturnData {
            program_id: Pubkey::new_unique(),
            data: vec![1, 2, 3],
        };
        let compute_units_consumed_1 = Some(3812649u64);
        let compute_units_consumed_2 = Some(42u64);

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
            loaded_addresses: test_loaded_addresses,
            return_data: Some(test_return_data.clone()),
            compute_units_consumed: compute_units_consumed_1,
        }
    }

    fn create_confirmed_transaction(
        signature: Signature,
        slot: Slot,
        fee: u64,
        block_time: Option<UnixTimestamp>,
        tx_signatures: Option<Vec<Signature>>,
    ) -> (ConfirmedTransactionWithStatusMeta, SanitizedTransaction) {
        let meta = create_transaction_status_meta(fee);
        let writable_addresses = meta.loaded_addresses.writable.clone();
        let readonly_addresses = meta.loaded_addresses.readonly.clone();
        let num_readonly_unsigned_accounts = readonly_addresses.len() as u8 - 1;
        let signatures = tx_signatures.unwrap_or_else(|| {
            vec![Signature::new_unique(), Signature::new_unique()]
        });
        let msg = v0::Message {
            account_keys: [writable_addresses, readonly_addresses].concat(),
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

    #[test]
    fn test_persist_block_time() {
        init_logger!();

        let ledger_path = get_tmp_ledger_path_auto_delete!();
        let store = Store::open(ledger_path.path()).unwrap();

        let slot_0 = 5;
        let slot_1 = slot_0 + 1;
        let slot_2 = slot_1 + 1;

        assert!(store.cache_block_time(0, slot_0).is_ok());
        assert!(store.cache_block_time(1, slot_1).is_ok());
        assert!(store.cache_block_time(2, slot_2).is_ok());

        assert_eq!(store.get_block_time(0).unwrap().unwrap(), slot_0);
        assert_eq!(store.get_block_time(1).unwrap().unwrap(), slot_1);
        assert_eq!(store.get_block_time(2).unwrap().unwrap(), slot_2);
    }

    #[test]
    fn test_persist_transaction_status() {
        init_logger!();

        let ledger_path = get_tmp_ledger_path_auto_delete!();
        let store = Store::open(ledger_path.path()).unwrap();

        // First Case
        {
            let (signature, slot) = (Signature::default(), 0);

            // result not found
            assert!(store
                .read_transaction_status((Signature::default(), 0))
                .unwrap()
                .is_none());

            // insert value
            let meta = create_transaction_status_meta(5);
            assert!(store
                .write_transaction_status(slot, signature, meta.clone(), 0,)
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
            let meta = create_transaction_status_meta(9);
            assert!(store
                .write_transaction_status(slot, signature, meta.clone(), 0,)
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
        let store = Store::open(ledger_path.path()).unwrap();

        let (sig_uno, slot_uno) = (Signature::default(), 0);
        let (sig_dos, slot_dos) = (Signature::from([2u8; 64]), 9);

        // result not found
        assert!(store
            .read_transaction_status((Signature::default(), 0))
            .unwrap()
            .is_none());

        // insert value
        let status_uno = create_transaction_status_meta(5);
        assert!(store
            .write_transaction_status(slot_uno, sig_uno, status_uno.clone(), 0,)
            .is_ok());

        // Finds by matching signature
        {
            let (slot, status) =
                store.get_transaction_status(sig_uno, 0).unwrap().unwrap();
            assert_eq!(slot, slot_uno);
            assert_eq!(status, status_uno);

            // Does not find it by other signature
            assert!(store
                .get_transaction_status(sig_dos, 0)
                .unwrap()
                .is_none());
        }

        // Add a status for the other signature
        let status_dos = create_transaction_status_meta(5);
        assert!(store
            .write_transaction_status(slot_dos, sig_dos, status_dos.clone(), 0,)
            .is_ok());

        // First still there
        {
            let (slot, status) =
                store.get_transaction_status(sig_uno, 0).unwrap().unwrap();
            assert_eq!(slot, slot_uno);
            assert_eq!(status, status_uno);
        }

        // Second one is found now as well
        {
            let (slot, status) =
                store.get_transaction_status(sig_dos, 0).unwrap().unwrap();
            assert_eq!(slot, slot_dos);
            assert_eq!(status, status_dos);
        }
    }

    #[test]
    fn test_get_complete_transaction_by_signature() {
        init_logger!();

        let ledger_path = get_tmp_ledger_path_auto_delete!();
        let store = Store::open(ledger_path.path()).unwrap();

        let (sig_uno, slot_uno, block_time_uno) =
            (Signature::default(), 0, 100);
        let (sig_dos, slot_dos, block_time_dos) =
            (Signature::from([2u8; 64]), 9, 200);

        let (tx_uno, sanitized_uno) = create_confirmed_transaction(
            sig_uno,
            slot_uno,
            5,
            Some(block_time_uno),
            None,
        );

        let (tx_dos, sanitized_dos) = create_confirmed_transaction(
            sig_dos,
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
        assert!(store.cache_block_time(slot_uno, block_time_uno).is_ok());

        // Get first transaction by signature providing low enough slot
        let tx = store.get_complete_transaction(sig_uno, 0).unwrap().unwrap();
        assert_eq!(tx, tx_uno);

        // Get first transaction by signature providing slot that's too high
        assert!(store
            .get_complete_transaction(sig_uno, slot_uno + 1)
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
        assert!(store.cache_block_time(slot_dos, block_time_dos).is_ok());

        // Get second transaction by signature providing slot at which it was stored
        let tx = store
            .get_complete_transaction(sig_dos, slot_dos)
            .unwrap()
            .unwrap();
        assert_eq!(tx, tx_dos);
    }

    #[test]
    fn test_find_address_signatures() {
        init_logger!();

        let ledger_path = get_tmp_ledger_path_auto_delete!();
        let store = Store::open(ledger_path.path()).unwrap();

        // 1. Add some transaction statuses
        // All addresses from a previous status are included in the next one to test
        // addresses that are part of multiple transactions
        let (signature_uno, slot_uno) = (Signature::new_unique(), 10);
        let (read_uno, write_uno) = {
            let meta = create_transaction_status_meta(5);
            let read_uno = meta.loaded_addresses.readonly[0];
            let write_uno = meta.loaded_addresses.writable[0];
            assert!(store
                .write_transaction_status(
                    slot_uno,
                    signature_uno,
                    meta.clone(),
                    0,
                )
                .is_ok());
            (read_uno, write_uno)
        };

        let (signature_dos, slot_dos) = (Signature::new_unique(), 20);
        let signature_dos_2 = Signature::new_unique();
        let (read_dos, write_dos) = {
            let mut meta = create_transaction_status_meta(5);
            let read_dos = meta.loaded_addresses.readonly[0];
            let write_dos = meta.loaded_addresses.writable[0];
            meta.loaded_addresses.readonly.push(read_uno);
            meta.loaded_addresses.writable.push(write_uno);
            assert!(store
                .write_transaction_status(
                    slot_dos,
                    signature_dos,
                    meta.clone(),
                    0,
                )
                .is_ok());

            // read_dos and write_dos are part of another transaction in the same slot
            let mut meta = create_transaction_status_meta(8);
            meta.loaded_addresses.readonly.push(read_dos);
            meta.loaded_addresses.writable.push(write_dos);
            assert!(store
                .write_transaction_status(
                    slot_dos,
                    signature_dos_2,
                    meta.clone(),
                    1,
                )
                .is_ok());

            (read_dos, write_dos)
        };

        let (signature_tres, slot_tres) = (Signature::new_unique(), 30);
        let (read_tres, write_tres) = {
            let mut meta = create_transaction_status_meta(5);
            let read_tres = meta.loaded_addresses.readonly[0];
            let write_tres = meta.loaded_addresses.writable[0];
            meta.loaded_addresses.readonly.push(read_uno);
            meta.loaded_addresses.writable.push(write_uno);
            meta.loaded_addresses.readonly.push(read_dos);
            meta.loaded_addresses.writable.push(write_dos);

            assert!(store
                .write_transaction_status(
                    slot_tres,
                    signature_tres,
                    meta.clone(),
                    0,
                )
                .is_ok());
            (read_tres, write_tres)
        };

        // 2. Find them providing slot
        {
            let results = store
                .find_address_signatures_for_slot(write_uno, slot_uno)
                .unwrap();
            assert_eq!(results.len(), 1);
            assert_eq!(results[0].0, slot_uno);
            assert_eq!(results[0].1, signature_uno);

            let results = store
                .find_address_signatures_for_slot(write_uno, slot_tres)
                .unwrap();
            assert_eq!(results.len(), 1);
            assert_eq!(results[0].0, slot_tres);
            assert_eq!(results[0].1, signature_tres);

            let results = store
                .find_address_signatures_for_slot(read_dos, slot_dos)
                .unwrap();
            assert_eq!(results.len(), 2);
            assert_eq!(results[0].0, slot_dos);
            assert_eq!(results[0].1, signature_dos);
            assert_eq!(results[1].0, slot_dos);
            assert_eq!(results[1].1, signature_dos_2);

            assert!(store
                .find_address_signatures_for_slot(read_dos, slot_uno)
                .unwrap()
                .is_empty());
        }

        // 3. Add a few more transactions
        let (signature_cuatro, slot_cuatro) = (Signature::new_unique(), 31);
        let (read_cuatro, write_cuatro) = {
            let mut meta = create_transaction_status_meta(5);
            let read_cuatro = meta.loaded_addresses.readonly[0];
            let write_cuatro = meta.loaded_addresses.writable[0];
            assert!(store
                .write_transaction_status(
                    slot_cuatro,
                    signature_cuatro,
                    meta.clone(),
                    0,
                )
                .is_ok());
            (read_cuatro, write_cuatro)
        };

        let (signature_cinco, slot_cinco) = (Signature::new_unique(), 31);
        let (read_cinco, write_cinco) = {
            let mut meta = create_transaction_status_meta(5);
            let read_cinco = meta.loaded_addresses.readonly[0];
            let write_cinco = meta.loaded_addresses.writable[0];
            assert!(store
                .write_transaction_status(
                    slot_cinco,
                    signature_cinco,
                    meta.clone(),
                    0,
                )
                .is_ok());
            (read_cinco, write_cinco)
        };

        let (signature_seis, slot_seis) = (Signature::new_unique(), 32);
        let (read_seis, write_seis) = {
            let mut meta = create_transaction_status_meta(5);
            let read_seis = meta.loaded_addresses.readonly[0];
            let write_seis = meta.loaded_addresses.writable[0];
            meta.loaded_addresses.readonly.push(read_uno);
            meta.loaded_addresses.writable.push(write_uno);
            assert!(store
                .write_transaction_status(
                    slot_seis,
                    signature_seis,
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

        // 3. Fill in block times
        assert!(store.cache_block_time(slot_uno, 1).is_ok());
        assert!(store.cache_block_time(slot_dos, 2).is_ok());
        assert!(store.cache_block_time(slot_tres, 3).is_ok());
        assert!(store.cache_block_time(slot_cuatro, 4).is_ok());
        assert!(store.cache_block_time(slot_cinco, 5).is_ok());
        assert!(store.cache_block_time(slot_seis, 6).is_ok());

        // 4. Find signatures for address with default limits
        let res = store
            .get_confirmed_signatures_for_address(
                read_cuatro,
                slot_seis,
                None,
                None,
                1000,
            )
            .unwrap();
        assert!(!res.found_before);
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

        // 5. Find signatures with before/until configs
        {
            fn extract(
                infos: Vec<ConfirmedTransactionStatusWithSignature>,
            ) -> Vec<(Slot, Signature)> {
                infos.into_iter().map(|x| (x.slot, x.signature)).collect()
            }
            // No before/after
            let sigs = extract(
                store
                    .get_confirmed_signatures_for_address(
                        read_uno, slot_seis, None, None, 1000,
                    )
                    .unwrap()
                    .infos,
            );
            assert!(!res.found_before);
            assert_eq!(
                sigs,
                vec![
                    (slot_seis, signature_seis),
                    (slot_tres, signature_tres),
                    (slot_dos, signature_dos),
                    (slot_uno, signature_uno),
                ]
            );

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
            assert!(res.found_before);
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
            assert!(res.found_before);
            assert_eq!(
                extract(res.infos.clone()),
                vec![
                    (slot_tres, signature_tres),
                    (slot_dos, signature_dos),
                    (slot_uno, signature_uno),
                ]
            );
        }
    }
}
