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
use crate::database::iterator::IteratorMode;
use crate::database::ledger_column::LedgerColumn;
use crate::database::meta::TransactionStatusIndexMeta;
use crate::database::options::LedgerOptions;

use crate::errors::LedgerError;
use crate::store::utils::adjust_ulimit_nofile;

pub struct Store {
    ledger_path: PathBuf,
    db: Arc<Database>,

    transaction_status_cf: LedgerColumn<cf::TransactionStatus>,
    transaction_status_index_cf: LedgerColumn<cf::TransactionStatusIndex>,

    highest_primary_index_slot: RwLock<Option<Slot>>,
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
        let transaction_status_index_cf = db.column();

        let db = Arc::new(db);

        // NOTE: left out max root

        measure.stop();
        info!("Opening blockstore done; {measure}");

        let blockstore = Store {
            ledger_path: ledger_path.to_path_buf(),
            db,
            transaction_status_cf,
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use solana_sdk::{
        instruction::CompiledInstruction,
        message::v0::LoadedAddresses,
        pubkey::Pubkey,
        signature::{Keypair, Signature},
        signer::Signer,
        transaction::TransactionError,
        transaction_context::TransactionReturnData,
    };
    use solana_transaction_status::{
        InnerInstruction, InnerInstructions, TransactionStatusMeta,
    };
    use tempfile::{Builder, TempDir};
    use test_tools_core::init_logger;

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

    #[test]
    fn test_persist_transaction_status() {
        init_logger!();

        let ledger_path = get_tmp_ledger_path_auto_delete!();
        let store = Store::open(ledger_path.path()).unwrap();

        let transaction_status_cf = &store.transaction_status_cf;

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

        // result not found
        assert!(transaction_status_cf
            .get_protobuf((Signature::default(), 0))
            .unwrap()
            .is_none());

        // insert value
        let status = TransactionStatusMeta {
            status: solana_sdk::transaction::Result::<()>::Err(
                TransactionError::AccountNotFound,
            ),
            fee: 5u64,
            pre_balances: pre_balances_vec.clone(),
            post_balances: post_balances_vec.clone(),
            inner_instructions: Some(inner_instructions_vec.clone()),
            log_messages: Some(log_messages_vec.clone()),
            pre_token_balances: Some(pre_token_balances_vec.clone()),
            post_token_balances: Some(post_token_balances_vec.clone()),
            rewards: Some(rewards_vec.clone()),
            loaded_addresses: test_loaded_addresses.clone(),
            return_data: Some(test_return_data.clone()),
            compute_units_consumed: compute_units_consumed_1,
        }
        .into();
        assert!(transaction_status_cf
            .put_protobuf((Signature::default(), 0), &status)
            .is_ok());

        // result found
        let TransactionStatusMeta {
            status,
            fee,
            pre_balances,
            post_balances,
            inner_instructions,
            log_messages,
            pre_token_balances,
            post_token_balances,
            rewards,
            loaded_addresses,
            return_data,
            compute_units_consumed,
        } = transaction_status_cf
            .get_protobuf((Signature::default(), 0))
            .unwrap()
            .unwrap()
            .try_into()
            .unwrap();
        assert_eq!(status, Err(TransactionError::AccountNotFound));
        assert_eq!(fee, 5u64);
        assert_eq!(pre_balances, pre_balances_vec);
        assert_eq!(post_balances, post_balances_vec);
        assert_eq!(inner_instructions.unwrap(), inner_instructions_vec);
        assert_eq!(log_messages.unwrap(), log_messages_vec);
        assert_eq!(pre_token_balances.unwrap(), pre_token_balances_vec);
        assert_eq!(post_token_balances.unwrap(), post_token_balances_vec);
        assert_eq!(rewards.unwrap(), rewards_vec);
        assert_eq!(loaded_addresses, test_loaded_addresses);
        assert_eq!(return_data.unwrap(), test_return_data);
        assert_eq!(compute_units_consumed, compute_units_consumed_1);

        // insert value
        let status = TransactionStatusMeta {
            status: solana_sdk::transaction::Result::<()>::Ok(()),
            fee: 9u64,
            pre_balances: pre_balances_vec.clone(),
            post_balances: post_balances_vec.clone(),
            inner_instructions: Some(inner_instructions_vec.clone()),
            log_messages: Some(log_messages_vec.clone()),
            pre_token_balances: Some(pre_token_balances_vec.clone()),
            post_token_balances: Some(post_token_balances_vec.clone()),
            rewards: Some(rewards_vec.clone()),
            loaded_addresses: test_loaded_addresses.clone(),
            return_data: Some(test_return_data.clone()),
            compute_units_consumed: compute_units_consumed_2,
        }
        .into();
        assert!(transaction_status_cf
            .put_protobuf((Signature::from([2u8; 64]), 9), &status,)
            .is_ok());

        // result found
        let TransactionStatusMeta {
            status,
            fee,
            pre_balances,
            post_balances,
            inner_instructions,
            log_messages,
            pre_token_balances,
            post_token_balances,
            rewards,
            loaded_addresses,
            return_data,
            compute_units_consumed,
        } = transaction_status_cf
            .get_protobuf((Signature::from([2u8; 64]), 9))
            .unwrap()
            .unwrap()
            .try_into()
            .unwrap();

        // deserialize
        assert_eq!(status, Ok(()));
        assert_eq!(fee, 9u64);
        assert_eq!(pre_balances, pre_balances_vec);
        assert_eq!(post_balances, post_balances_vec);
        assert_eq!(inner_instructions.unwrap(), inner_instructions_vec);
        assert_eq!(log_messages.unwrap(), log_messages_vec);
        assert_eq!(pre_token_balances.unwrap(), pre_token_balances_vec);
        assert_eq!(post_token_balances.unwrap(), post_token_balances_vec);
        assert_eq!(rewards.unwrap(), rewards_vec);
        assert_eq!(loaded_addresses, test_loaded_addresses);
        assert_eq!(return_data.unwrap(), test_return_data);
        assert_eq!(compute_units_consumed, compute_units_consumed_2);
    }
}
