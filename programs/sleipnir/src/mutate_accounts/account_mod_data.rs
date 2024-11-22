use sleipnir_core::traits::PersistsAccountModData;
use std::{
    collections::{HashMap, HashSet},
    sync::{
        atomic::{AtomicUsize, Ordering},
        RwLock,
    },
};

use lazy_static::lazy_static;

lazy_static! {
    /// In order to modify large data chunks we cannot include all the data as part of the
    /// transaction.
    /// Instead we register data here _before_ invoking the actual instruction and when it is
    /// processed it resolved that data from the key that we provide in its place.
    static ref DATA_MODS: RwLock<HashMap<usize, Vec<u8>>> = RwLock::new(HashMap::new());

    static ref PERSISTER: RwLock<Option<Box<dyn PersistsAccountModData>>> = RwLock::new(None);
}

pub fn get_account_mod_data_id() -> usize {
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    COUNTER.fetch_add(1, Ordering::Relaxed)
}

pub(crate) fn set_account_mod_data(data: Vec<u8>) -> usize {
    let id = get_account_mod_data_id();
    DATA_MODS
        .write()
        .expect("DATA_MODS poisoned")
        .insert(id, data);
    id
}

pub(super) fn get_data(id: usize) -> Option<Vec<u8>> {
    DATA_MODS.write().expect("DATA_MODS poisoned").remove(&id)
}

pub fn init_persister<T: PersistsAccountModData>(persister: T) {
    PERSISTER
        .write()
        .expect("PERSISTER poisoned")
        .replace(Box::new(persister));
}

fn load_data(id: usize) -> Result<Option<Vec<u8>>, Box<dyn std::error::Error>> {
    PERSISTER
        .read()
        .expect("PERSISTER poisoned")
        .as_ref()
        .ok_or("AccounModPersister needs to be set on startup")?
        .load(id)
}

fn persist_data(
    id: usize,
    data: &[u8],
) -> Result<(), Box<dyn std::error::Error>> {
    PERSISTER
        .read()
        .expect("PERSISTER poisoned")
        .as_ref()
        .ok_or("AccounModPersister needs to be set on startup")?
        .persist(id, data)
}
