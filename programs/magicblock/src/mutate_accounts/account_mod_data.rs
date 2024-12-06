use std::{
    collections::HashMap,
    ops::Neg,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex, RwLock,
    },
};

use lazy_static::lazy_static;
use magicblock_core::traits::PersistsAccountModData;
use solana_program_runtime::{ic_msg, invoke_context::InvokeContext};

use crate::{magicblock_instruction::MagicBlockProgramError, validator};

lazy_static! {
    /// In order to modify large data chunks we cannot include all the data as part of the
    /// transaction.
    /// Instead we register data here _before_ invoking the actual instruction and when it is
    /// processed it resolved that data from the key that we provide in its place.
    static ref DATA_MODS: Mutex<HashMap<u64, Vec<u8>>> = Mutex::new(HashMap::new());

    /// In order to support replaying transactions we need to persist the data that is
    /// loaded from the [DATA_MODS]
    /// During replay the [DATA_MODS] won't have the data for the particular id in which
    /// case it is loaded via the persister instead.
    static ref PERSISTER: RwLock<Option<Arc<dyn PersistsAccountModData>>> = RwLock::new(None);

    static ref DATA_MOD_ID: AtomicU64 = AtomicU64::new(0);

    static ref MAX_REPLAY_DATA_MOD_ID: AtomicU64 = AtomicU64::new(0);
}

/// We capture the max id we see during ledger replay and use it to assign
/// it to the [DATA_MOD_ID] once the validator starts running.
/// As a result once the [DATA_MOD_ID] has the same value as it did
/// when the initial validator instance stopped.
fn update_max_seen_data_id(next_id: u64) {
    MAX_REPLAY_DATA_MOD_ID.fetch_max(next_id, Ordering::Relaxed);
}

pub fn get_account_mod_data_id() -> u64 {
    static ASSIGNED_MAX_ID: AtomicBool = AtomicBool::new(false);

    if !ASSIGNED_MAX_ID.swap(true, Ordering::Relaxed) {
        DATA_MOD_ID.store(
            MAX_REPLAY_DATA_MOD_ID.load(Ordering::Relaxed) + 1,
            Ordering::Relaxed,
        );
    }

    DATA_MOD_ID.fetch_add(1, Ordering::Relaxed)
}

pub(crate) fn set_account_mod_data(data: Vec<u8>) -> u64 {
    let id = get_account_mod_data_id();
    DATA_MODS
        .lock()
        .expect("DATA_MODS poisoned")
        .insert(id, data);
    // update metrics related to total count of data mods
    magicblock_metrics::metrics::adjust_active_data_mods(1);
    id
}

pub(super) fn get_data(id: u64) -> Option<Vec<u8>> {
    DATA_MODS
        .lock()
        .expect("DATA_MODS poisoned")
        .remove(&id)
        .inspect(|v| {
            // decrement metrics
            let len = (v.len() as i64).neg();
            magicblock_metrics::metrics::adjust_active_data_mods_size(len);
            magicblock_metrics::metrics::adjust_active_data_mods(-1);
        })
}

pub fn init_persister<T: PersistsAccountModData>(persister: Arc<T>) {
    PERSISTER
        .write()
        .expect("PERSISTER poisoned")
        .replace(persister);
}

pub fn persister_info() -> String {
    PERSISTER
        .read()
        .expect("PERSISTER poisoned")
        .as_ref()
        .map(|p| p.to_string())
        .unwrap_or_else(|| "None".to_string())
}

fn load_data(id: u64) -> Result<Option<Vec<u8>>, Box<dyn std::error::Error>> {
    PERSISTER
        .read()
        .expect("PERSISTER poisoned")
        .as_ref()
        .ok_or("AccountModPersister needs to be set on startup")?
        .load(id)
}

fn persist_data(
    id: u64,
    data: Vec<u8>,
) -> Result<(), Box<dyn std::error::Error>> {
    PERSISTER
        .read()
        .expect("PERSISTER poisoned")
        .as_ref()
        .ok_or("AccounModPersister needs to be set on startup")?
        .persist(id, data)
}

/// The resolved data including an indication about how it was resolved.
pub(super) enum ResolvedAccountModData {
    /// The data was resolved from memory while the validator was processing
    /// mutation transactions.
    FromMemory { id: u64, data: Vec<u8> },
    /// The data was resolved from the ledger while replaying transactions.
    FromStorage { id: u64, data: Vec<u8> },
    /// The data was not found in either memory or storage which means the
    /// transaction is invalid.
    NotFound { id: u64 },
}

impl ResolvedAccountModData {
    pub fn id(&self) -> u64 {
        use ResolvedAccountModData::*;
        match self {
            FromMemory { id, .. } => *id,
            FromStorage { id, .. } => *id,
            NotFound { id } => *id,
        }
    }

    pub fn data(&self) -> Option<&[u8]> {
        use ResolvedAccountModData::*;
        match self {
            FromMemory { data, .. } => Some(data),
            FromStorage { data, .. } => Some(data),
            NotFound { .. } => None,
        }
    }

    pub fn persist(
        self,
        invoke_context: &InvokeContext,
    ) -> Result<(), MagicBlockProgramError> {
        use ResolvedAccountModData::*;
        let (id, data) = match self {
            FromMemory { id, data } => (id, data),
            FromStorage { id, .. } => {
                ic_msg!(
                    invoke_context,
                    "MutateAccounts: trying to persist data that came from storage with id: {}",
                    id
                );
                return Err(
                    MagicBlockProgramError::AttemptedToPersistDataFromStorage,
                );
            }
            // Even though it is a developer error to call this method on NotFound
            // we don't panic here, but let the mutate transaction fail by returning
            // an error result.
            NotFound { id } => {
                ic_msg!(
                    invoke_context,
                    "MutateAccounts: trying to persist unresolved with id: {}",
                    id
                );
                return Err(
                    MagicBlockProgramError::AttemptedToPersistUnresolvedData,
                );
            }
        };

        persist_data(id, data).map_err(|err| {
            ic_msg!(
                invoke_context,
                "MutateAccounts: failed to persist account mod data: {}",
                err.to_string()
            );
            MagicBlockProgramError::FailedToPersistAccountModData
        })?;

        Ok(())
    }

    pub fn is_from_memory(&self) -> bool {
        matches!(self, ResolvedAccountModData::FromMemory { .. })
    }
}

pub(super) fn resolve_account_mod_data(
    id: u64,
    invoke_context: &InvokeContext,
) -> Result<ResolvedAccountModData, MagicBlockProgramError> {
    if let Some(data) = get_data(id) {
        Ok(ResolvedAccountModData::FromMemory { id, data })
    } else if validator::is_starting_up() {
        match load_data(id).map_err(|err| {
            ic_msg!(
                invoke_context,
                "MutateAccounts: failed to load account mod data: {}",
                err.to_string()
            );
            MagicBlockProgramError::AccountDataResolutionFailed
        })? {
            Some(data) => {
                update_max_seen_data_id(id);
                Ok(ResolvedAccountModData::FromStorage { id, data })
            }
            None => Ok(ResolvedAccountModData::NotFound { id }),
        }
    } else {
        // We only load account data from the ledger while we are replaying transactions
        // from that ledger.
        // Afterwards the data needs to be added to the memory map before running the
        // transaction.
        ic_msg!(
            invoke_context,
            "MutateAccounts: failed to load account mod data: {} from memory after validator started up",
            id,
        );
        Err(MagicBlockProgramError::AccountDataMissingFromMemory)
    }
}
