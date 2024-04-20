use std::{
    collections::HashSet,
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
    ConfirmedTransactionWithStatusMeta, TransactionStatusMeta,
    TransactionWithStatusMeta, VersionedTransactionWithStatusMeta,
};

use crate::{
    conversions::confirmed_transaction,
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

pub struct Store {
    ledger_path: PathBuf,
    db: Arc<Database>,

    transaction_status_cf: LedgerColumn<cf::TransactionStatus>,
    address_signatures_cf: LedgerColumn<cf::AddressSignatures>,
    transaction_status_index_cf: LedgerColumn<cf::TransactionStatusIndex>,
    blocktime_cf: LedgerColumn<cf::Blocktime>,
    confirmed_transaction: LedgerColumn<cf::ConfirmedTransaction>,

    highest_primary_index_slot: RwLock<Option<Slot>>,

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
        let confirmed_transaction = db.column();

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
            confirmed_transaction,

            highest_primary_index_slot: RwLock::<Option<Slot>>::default(),

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
    }

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
        // let _lock = self.check_lowest_cleanup_slot(slot)?;
        self.blocktime_cf.get(slot)
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
                let tx =
                    confirmed_transaction::from_generated_confirmed_transaction(
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

        let mut iterator = self
            .confirmed_transaction
            .iter_current_index_filtered(IteratorMode::From(
                (signature, highest_confirmed_slot),
                IteratorDirection::Forward,
            ))?;

        let confirmed_transaction = match iterator.next() {
            Some(((signature, slot), _data)) => self
                .confirmed_transaction
                .get_protobuf((signature, slot))?
                .map(|tx| (slot, tx)),
            None => None,
        };
        Ok(confirmed_transaction)
    }

    /// Writes a confirmed transaction pieced together from the provided inputs
    /// * `signature` - Signature of the transaction
    /// * `slot` - Slot at which the transaction was confirmed
    /// * `transaction` - Transaction to be written, we take a SanititizedTransaction here
    ///                   since that is what we get provided by Geyser
    /// * `status` - status of the transaction
    pub fn write_confirmed_transaction(
        &self,
        signature: Signature,
        slot: Slot,
        transaction: SanitizedTransaction,
        status: TransactionStatusMeta,
    ) -> LedgerResult<()> {
        let versioned = transaction.to_versioned_transaction();
        let tx = generated::ConfirmedTransaction {
            transaction: Some(versioned.into()),
            meta: Some(status.into()),
        };
        self.confirmed_transaction
            .put_protobuf((signature, slot), &tx)?;
        Ok(())
    }

    fn read_confirmed_transaction(
        &self,
        index: (Signature, Slot),
    ) -> LedgerResult<Option<generated::ConfirmedTransaction>> {
        let result = self.confirmed_transaction.get_protobuf(index)?;
        Ok(result)
    }

    // -----------------
    // TransactionStatus
    // -----------------
    /// Returns a transaction status
    pub fn get_transaction_status(
        &self,
        signature: Signature,
    ) -> LedgerResult<Option<(Slot, TransactionStatusMeta)>> {
        self.rpc_api_metrics
            .num_get_transaction_status
            .fetch_add(1, Ordering::Relaxed);

        let mut iterator =
            self.transaction_status_cf.iter_current_index_filtered(
                IteratorMode::From((signature, 0), IteratorDirection::Forward),
            )?;

        let result = match iterator.next() {
            Some(((signature, slot), _data)) => self
                .transaction_status_cf
                .get_protobuf((signature, slot))?
                .and_then(|status| status.try_into().ok())
                .map(|status| (slot, status)),
            None => None,
        };
        Ok(result)
    }

    pub fn read_transaction_status(
        &self,
        index: (Signature, Slot),
    ) -> LedgerResult<Option<TransactionStatusMeta>> {
        let result = self.transaction_status_cf.get_protobuf(index)?;
        Ok(result.and_then(|meta| meta.try_into().ok()))
    }

    pub fn write_transaction_status(
        &self,
        slot: Slot,
        signature: Signature,
        writable_keys: Vec<&Pubkey>,
        readonly_keys: Vec<&Pubkey>,
        status: TransactionStatusMeta,
        transaction_index: usize,
    ) -> LedgerResult<()> {
        let status = status.into();
        let transaction_index = u32::try_from(transaction_index)
            .map_err(|_| LedgerError::TransactionIndexOverflow)?;
        self.transaction_status_cf
            .put_protobuf((signature, slot), &status)?;
        for address in writable_keys {
            self.address_signatures_cf.put(
                (*address, slot, transaction_index, signature),
                &AddressSignatureMeta { writeable: true },
            )?;
        }
        for address in readonly_keys {
            self.address_signatures_cf.put(
                (*address, slot, transaction_index, signature),
                &AddressSignatureMeta { writeable: false },
            )?;
        }
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
            let writable_addresses = meta.loaded_addresses.writable.clone();
            let readonly_addresses = meta.loaded_addresses.writable.clone();
            assert!(store
                .write_transaction_status(
                    slot,
                    signature,
                    writable_addresses.iter().collect(),
                    readonly_addresses.iter().collect(),
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
            let meta = create_transaction_status_meta(9);
            let writable_addresses = meta.loaded_addresses.writable.clone();
            let readonly_addresses = meta.loaded_addresses.writable.clone();
            assert!(store
                .write_transaction_status(
                    slot,
                    signature,
                    writable_addresses.iter().collect(),
                    readonly_addresses.iter().collect(),
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
        let writable_addresses = status_uno.loaded_addresses.writable.clone();
        let readonly_addresses = status_uno.loaded_addresses.writable.clone();
        assert!(store
            .write_transaction_status(
                slot_uno,
                sig_uno,
                writable_addresses.iter().collect(),
                readonly_addresses.iter().collect(),
                status_uno.clone(),
                0,
            )
            .is_ok());

        // Finds by matching signature
        {
            let (slot, status) =
                store.get_transaction_status(sig_uno).unwrap().unwrap();
            assert_eq!(slot, slot_uno);
            assert_eq!(status, status_uno);

            // Does not find it by other signature
            assert!(store.get_transaction_status(sig_dos).unwrap().is_none());
        }

        // Add a status for the other signature
        let status_dos = create_transaction_status_meta(5);
        let writable_addresses = status_dos.loaded_addresses.writable.clone();
        let readonly_addresses = status_dos.loaded_addresses.writable.clone();
        assert!(store
            .write_transaction_status(
                slot_dos,
                sig_dos,
                writable_addresses.iter().collect(),
                readonly_addresses.iter().collect(),
                status_dos.clone(),
                0,
            )
            .is_ok());

        // First still there
        {
            let (slot, status) =
                store.get_transaction_status(sig_uno).unwrap().unwrap();
            assert_eq!(slot, slot_uno);
            assert_eq!(status, status_uno);
        }

        // Second one is found now as well
        {
            let (slot, status) =
                store.get_transaction_status(sig_dos).unwrap().unwrap();
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
        // TODO: write blocktime

        // 0. Neither transaction is in the store
        assert!(store
            .get_confirmed_transaction(sig_uno, 100)
            .unwrap()
            .is_none());
        assert!(store
            .get_confirmed_transaction(sig_dos, 200)
            .unwrap()
            .is_none());

        // 1. Write first transaction
        store.write_confirmed_transaction(
            sig_uno,
            slot_uno,
            sanitized_uno.clone(),
            tx_uno.tx_with_meta.get_status_meta().unwrap(),
        );

        let tx = store.get_complete_transaction(sig_uno, 0).unwrap().unwrap();
        debug!("{:#?}", tx);
        debug!("{:#?}", tx_uno);
        // assert_eq!(tx.slot, slot_uno);
        // assert_eq!(tx, tx_uno);
    }
}
