// NOTE: copied from runtime/src/status_cache.rs
// NOTE: most likely our implementation can be greatly simplified since we don't
// support forks

use std::{
    cmp,
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex},
};

use log::*;
use rand::{thread_rng, Rng};
use solana_frozen_abi_macro::AbiExample;
use solana_sdk::{clock::Slot, hash::Hash, signature::Signature};

const CACHED_KEY_SIZE: usize = 20;
// Store forks in a single chunk of memory to avoid another lookup.
pub type ForkStatus<T> = Vec<(Slot, T)>;
type KeySlice = [u8; CACHED_KEY_SIZE];
type KeyMap<T> = HashMap<KeySlice, ForkStatus<T>>;

// A Map of hash + the highest fork it's been observed on along with
// the key offset and a Map of the key slice + Fork status for that key
type KeyStatusMap<T> = HashMap<Hash, (Slot, usize, KeyMap<T>)>;
type SlotTransactionStatuses<T> = Vec<(Slot, HashMap<Signature, T>)>;

// Map of Hash and status
pub type Status<T> = Arc<Mutex<HashMap<Hash, (usize, Vec<(KeySlice, T)>)>>>;
// A map of keys recorded in each fork; used to serialize for snapshots easily.
// Doesn't store a `SlotDelta` in it because the bool `root` is usually set much later
type SlotDeltaMap<T> = HashMap<Slot, Status<T>>;

#[derive(Clone, Debug, AbiExample)]
pub struct StatusCache<T: Clone> {
    cache_by_blockhash: KeyStatusMap<T>,
    transaction_status_cache: SlotTransactionStatuses<T>,
    roots: HashSet<Slot>,

    /// all keys seen during a fork/slot
    slot_deltas: SlotDeltaMap<T>,
    max_cache_entries: u64,
}

impl<T: Clone> StatusCache<T> {
    pub fn new(max_age: usize) -> Self {
        Self {
            cache_by_blockhash: HashMap::default(),
            transaction_status_cache: vec![],
            // 0 is always a root
            roots: HashSet::from([0]),
            slot_deltas: HashMap::default(),
            max_cache_entries: max_age as u64,
        }
    }

    // -----------------
    // Queries
    // -----------------
    pub fn get_recent_transaction_status(
        &self,
        signature: &Signature,
        lookback_slots: Option<Slot>,
    ) -> Option<(Slot, T)> {
        #[inline]
        fn handle_iter<'a, T, I>(
            signature: &Signature,
            lookback_slots: Slot,
            iter: I,
        ) -> Option<(Slot, T)>
        where
            T: Clone + 'a,
            I: Iterator<Item = &'a (Slot, HashMap<Signature, T>)>,
        {
            for (slot, map) in iter {
                if let Some(needle) = map.get(signature) {
                    return Some((*slot, needle.clone()));
                }
            }
            warn!(
                "Missed tx status from cache for '{}', lookback={}",
                signature, lookback_slots
            );
            None
        }

        let iter = self.transaction_status_cache.iter().rev();
        if let Some(lookback_slots) = lookback_slots {
            handle_iter(
                signature,
                lookback_slots,
                iter.take(lookback_slots as usize),
            )
        } else {
            handle_iter(signature, u64::MAX, iter)
        }
    }

    // -----------------
    // Inserts
    // -----------------
    pub fn insert_transaction_status(
        &mut self,
        slot: Slot,
        signature: &Signature,
        status: T,
    ) {
        // Either add a new transaction status entry for the slot or update the latest one
        // NOTE: that slot starts at 0
        if self.transaction_status_cache.len() <= slot as usize {
            self.transaction_status_cache.push((slot, HashMap::new()));
        }
        let (status_slot, map) =
            self.transaction_status_cache.last_mut().unwrap();
        debug_assert_eq!(*status_slot, slot);
        map.insert(*signature, status);
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
        let hash_map = self
            .cache_by_blockhash
            .entry(*transaction_blockhash)
            .or_insert_with(|| {
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
        let hash_map = self
            .cache_by_blockhash
            .entry(*transaction_blockhash)
            .or_insert((slot, key_index, HashMap::new()));
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

    /// Add a known root fork.  Roots are always valid ancestors.
    /// After MAX_CACHE_ENTRIES, roots are removed, and any old keys are cleared.
    pub fn add_root(&mut self, fork: Slot) {
        self.roots.insert(fork);
        self.purge_roots(fork);
    }

    // -----------------
    // Bookkeeping
    // -----------------

    /// Checks if the number slots we have seen (roots) and cached status for is larger
    /// than [MAX_CACHE_ENTRIES] (300). If so it does the following:
    ///
    /// 1. Removes smallest tracked slot from the currently tracked "roots"
    /// 2. Removes all status cache entries that are for that slot or older
    /// 3. Removes all slot deltas that are for that slot or older
    ///
    /// In Solana this check is performed any time a just rooted bank is squashed.
    ///
    /// We add a root on each slot advance instead.
    ///
    /// The terminology "roots" comes from the original Solana implementation which
    /// considered the banks that had been rooted.
    fn purge_roots(&mut self, slot: Slot) {
        // We allow the cache to grow to 1.5 the size of max cache entries
        // purging less regularly to reduce overhead.
        // At 50ms/slot we purge once per minute.
        if slot % (self.max_cache_entries / 2) == 0 {
            if slot <= self.max_cache_entries {
                return;
            }
            let min = slot - self.max_cache_entries;

            // At 50ms/slot lot every 5 seconds
            const LOG_CACHE_SIZE_INTERVAL: u64 = 20 * 5;
            let sizes_before = if log_enabled!(log::Level::Debug) {
                if slot % LOG_CACHE_SIZE_INTERVAL == 0 {
                    Some((
                        self.cache_by_blockhash
                            .iter()
                            .map(|(_, (_, _, m))| m.len())
                            .sum::<usize>(),
                        self.transaction_status_cache
                            .iter()
                            .map(|(_, m)| m.len())
                            .sum::<usize>(),
                    ))
                } else {
                    None
                }
            } else {
                None
            };
            self.roots.retain(|slot| *slot > min);
            self.cache_by_blockhash
                .retain(|_, (slot, _, _)| *slot > min);
            self.transaction_status_cache
                .retain(|(slot, _)| *slot > min);
            self.slot_deltas.retain(|slot, _| *slot > min);

            if let Some((cache_size_before, tx_status_size_before)) =
                sizes_before
            {
                let cache_size_after = self
                    .cache_by_blockhash
                    .iter()
                    .map(|(_, (_, _, m))| m.len())
                    .sum::<usize>();
                let tx_status_size_after = self
                    .transaction_status_cache
                    .iter()
                    .map(|(_, m)| m.len())
                    .sum::<usize>();
                log::debug!(
                    "Purged roots up to {}. Cache {} -> {}, TX Status {} -> {}",
                    min,
                    cache_size_before,
                    cache_size_after,
                    tx_status_size_before,
                    tx_status_size_after
                );
            }
        }
    }
}
