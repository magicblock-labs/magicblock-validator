// NOTE: copied from runtime/src/status_cache.rs
// NOTE: most likely our implementation can be greatly simplified since we don't
// support forks

use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex},
};

use log::trace;
use rand::{thread_rng, Rng};
use solana_accounts_db::ancestors::Ancestors;
use solana_frozen_abi_macro::AbiExample;
use solana_sdk::{clock::Slot, hash::Hash};

const CACHED_KEY_SIZE: usize = 20;
// Store forks in a single chunk of memory to avoid another lookup.
pub type ForkStatus<T> = Vec<(Slot, T)>;
type KeySlice = [u8; CACHED_KEY_SIZE];
type KeyMap<T> = HashMap<KeySlice, ForkStatus<T>>;

// A Map of hash + the highest fork it's been observed on along with
// the key offset and a Map of the key slice + Fork status for that key
type KeyStatusMap<T> = HashMap<Hash, (Slot, usize, KeyMap<T>)>;

// Map of Hash and status
pub type Status<T> = Arc<Mutex<HashMap<Hash, (usize, Vec<(KeySlice, T)>)>>>;
// A map of keys recorded in each fork; used to serialize for snapshots easily.
// Doesn't store a `SlotDelta` in it because the bool `root` is usually set much later
type SlotDeltaMap<T> = HashMap<Slot, Status<T>>;

#[derive(Clone, Debug, AbiExample)]
pub struct StatusCache<T: Clone> {
    cache: KeyStatusMap<T>,
    roots: HashSet<Slot>,

    /// all keys seen during a fork/slot
    slot_deltas: SlotDeltaMap<T>,
}

impl<T: Clone> Default for StatusCache<T> {
    fn default() -> Self {
        Self {
            cache: HashMap::default(),
            // 0 is always a root
            roots: HashSet::from([0]),
            slot_deltas: HashMap::default(),
        }
    }
}

impl<T: Clone> StatusCache<T> {
    /// Check if the key is in any of the forks in the ancestors set and
    /// with a certain blockhash.
    pub fn get_status<K: AsRef<[u8]>>(
        &self,
        key: K,
        transaction_blockhash: &Hash,
        ancestors: &Ancestors,
    ) -> Option<(Slot, T)> {
        let map = self.cache.get(transaction_blockhash)?;
        let (_, index, keymap) = map;
        let max_key_index =
            key.as_ref().len().saturating_sub(CACHED_KEY_SIZE + 1);
        let index = (*index).min(max_key_index);
        let key_slice: &[u8; CACHED_KEY_SIZE] =
            arrayref::array_ref![key.as_ref(), index, CACHED_KEY_SIZE];
        if let Some(stored_forks) = keymap.get(key_slice) {
            let res = stored_forks
                .iter()
                .find(|(f, _)| {
                    ancestors.contains_key(f) || self.roots.contains(f)
                })
                .cloned();
            if res.is_some() {
                return res;
            }
        }
        None
    }

    /// Search for a key with any blockhash
    /// Prefer get_status for performance reasons, it doesn't need
    /// to search all blockhashes.
    pub fn get_status_any_blockhash<K: AsRef<[u8]>>(
        &self,
        key: K,
        ancestors: &Ancestors,
    ) -> Option<(Slot, T)> {
        let keys: Vec<_> = self.cache.keys().copied().collect();

        for blockhash in keys.iter() {
            trace!("get_status_any_blockhash: trying {}", blockhash);
            let status = self.get_status(&key, blockhash, ancestors);
            if status.is_some() {
                return status;
            }
        }
        None
    }

    /// Insert a new key for a specific slot.
    pub fn insert<K: AsRef<[u8]>>(
        &mut self,
        transaction_blockhash: &Hash,
        key: K,
        slot: Slot,
        res: T,
    ) {
        let max_key_index =
            key.as_ref().len().saturating_sub(CACHED_KEY_SIZE + 1);
        let hash_map =
            self.cache.entry(*transaction_blockhash).or_insert_with(|| {
                let key_index = thread_rng().gen_range(0..max_key_index + 1);
                (slot, key_index, HashMap::new())
            });

        hash_map.0 = std::cmp::max(slot, hash_map.0);
        let key_index = hash_map.1.min(max_key_index);
        let mut key_slice = [0u8; CACHED_KEY_SIZE];
        key_slice.clone_from_slice(
            &key.as_ref()[key_index..key_index + CACHED_KEY_SIZE],
        );
        self.insert_with_slice(
            transaction_blockhash,
            slot,
            key_index,
            key_slice,
            res,
        );
    }

    fn insert_with_slice(
        &mut self,
        transaction_blockhash: &Hash,
        slot: Slot,
        key_index: usize,
        key_slice: [u8; CACHED_KEY_SIZE],
        res: T,
    ) {
        let hash_map = self.cache.entry(*transaction_blockhash).or_insert((
            slot,
            key_index,
            HashMap::new(),
        ));
        hash_map.0 = std::cmp::max(slot, hash_map.0);

        // NOTE: not supporting forks exactly, but need to insert the entry
        // In the future this cache can be simplified to be a map by blockhash only
        let forks = hash_map.2.entry(key_slice).or_default();
        forks.push((slot, res.clone()));
        let slot_deltas = self.slot_deltas.entry(slot).or_default();
        let mut fork_entry = slot_deltas.lock().unwrap();
        let (_, hash_entry) = fork_entry
            .entry(*transaction_blockhash)
            .or_insert((key_index, vec![]));
        hash_entry.push((key_slice, res))
    }

    pub(crate) fn clear_lte(&mut self, slot: Slot) {
        self.cache.retain(|_, (x, _, _)| *x > slot);
        self.roots.retain(|x| *x > slot);
        self.slot_deltas.retain(|x, _| *x > slot);
    }
}
