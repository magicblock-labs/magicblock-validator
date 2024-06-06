use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, RwLock},
};

#[derive(Debug, Clone)]
pub struct CountedEntry<V: Clone> {
    value: V,
    count: usize,
}

/// Can be anything, i.e. millis since a start date, slot number, etc.
type Timestamp = u64;

#[derive(Debug)]
pub struct TimestampedKey<K> {
    key: K,
    ts: Timestamp,
}

// -----------------
// SharedMap
// -----------------
/// Shared access to a [HashMap] wrapped in a [RwLock] and [Arc], but only
/// exposing query methods.
/// Consider it a limited interface for the [ExpiringHashMap].
#[derive(Debug)]
pub struct SharedMap<K, V>(Arc<RwLock<HashMap<K, CountedEntry<V>>>>)
where
    K: PartialEq + Eq + std::hash::Hash + Clone,
    V: Clone;

impl<K, V> SharedMap<K, V>
where
    K: PartialEq + Eq + std::hash::Hash + Clone,
    V: Clone,
{
    pub fn get(&self, key: &K) -> Option<V> {
        self.0
            .read()
            .expect("RwLock poisoned")
            .get(key)
            .map(|e| e.value.clone())
    }

    pub fn len(&self) -> usize {
        self.0.read().expect("RwLock poisoned").len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

// -----------------
// ExpiringHashMap
// -----------------
/// Wrapper around a [HashMap] that checks stored elements for expiration whenever a
/// new entry is inserted.
/// All elements that did expire are removed at that point.
#[derive(Debug)]
pub struct ExpiringHashMap<K, V>
where
    K: PartialEq + Eq + std::hash::Hash + Clone,
    V: Clone,
{
    map: Arc<RwLock<HashMap<K, CountedEntry<V>>>>,
    /// Buffer storing all keys ordered by their insertion time
    vec: Arc<RwLock<VecDeque<TimestampedKey<K>>>>,
    ttl: u64,
}

impl<K, V> ExpiringHashMap<K, V>
where
    K: PartialEq + Eq + std::hash::Hash + Clone,
    V: Clone,
{
    /// Creates a new ExpiringHashMap with the given max size.
    pub fn new(ttl: u64) -> Self {
        ExpiringHashMap {
            map: Arc::<RwLock<HashMap<K, CountedEntry<V>>>>::default(),
            vec: Arc::new(RwLock::new(VecDeque::new())),
            ttl,
        }
    }

    /// Insert a new key-value pair into the map and evict all expired entries.
    /// - *key* - The key at which to insert the value.
    /// - *value* - The value to insert.
    /// - *ts* - The current timestamp/slot
    pub fn insert(&self, key: K, value: V, ts: Timestamp) {
        // While inserting a new entry we ensure that any entries that expired are removed.

        // 1. Insert the new entry both into the map and the buffer tracking time stamps
        let entry = if let Some(mut entry) = self.map_remove(&key) {
            entry.count += 1;
            entry.value = value;
            entry
        } else {
            CountedEntry { value, count: 1 }
        };
        self.map_insert(key.clone(), entry);
        self.vec_push(TimestampedKey {
            key: key.clone(),
            ts,
        });

        // 2. Remove entries that expired unless they were updated more recently
        let n_keys_to_drain = {
            let vec = self.vec.read().expect("RwLock vec poisoned");
            let mut n = 0;
            // Find all keys up to the first one that isn't expired yet
            while let Some(ts_entry) = vec.get(n) {
                if ts_entry.ts + self.ttl > ts {
                    break;
                }
                n += 1;
            }
            n
        };

        // Remove the inserts from the buffer tracking timestamps
        let inserts_to_remove = if n_keys_to_drain > 0 {
            Some(
                self.vec
                    .write()
                    .expect("RwLock vec poisoned")
                    .drain(0..n_keys_to_drain)
                    .map(|e| e.key)
                    .collect::<Vec<_>>(),
            )
        } else {
            None
        };
        // Remove them from the map if they were the last insert for that key
        if let Some(inserts_to_remove) = inserts_to_remove {
            self.map_decrease_count_and_maybe_remove(&inserts_to_remove);
        }
    }

    pub fn shared_map(&self) -> SharedMap<K, V> {
        SharedMap(self.map.clone())
    }

    fn vec_push(&self, key: TimestampedKey<K>) {
        self.vec
            .write()
            .expect("RwLock vec poisoned")
            .push_back(key);
    }

    fn map_remove(&self, key: &K) -> Option<CountedEntry<V>> {
        self.map.write().expect("RwLock map poisoned").remove(key)
    }

    fn map_decrease_count_and_maybe_remove(&self, keys: &[K]) {
        // If a particular entry was updated multiple times it is present in our timestamp buffer
        // at multiple indexes. We want to remove it only once we find the last of those.
        let map = &mut self.map.write().expect("RwLock map poisoned");
        for key in keys {
            let remove = if let Some(entry) = map.get_mut(key) {
                entry.count -= 1;
                entry.count == 0
            } else {
                false
            };

            // This happens rarely for accounts that don't see updates for a long time
            if remove {
                map.remove(key);
            }
        }
    }

    fn map_contains_key(&self, key: &K) -> bool {
        self.map
            .read()
            .expect("RwLock map poisoned")
            .contains_key(key)
    }

    fn map_insert(&self, key: K, value: CountedEntry<V>) {
        self.map
            .write()
            .expect("RwLock map poisoned")
            .insert(key, value);
    }

    fn map_len(&self) -> usize {
        self.map.read().expect("RwLock map poisoned").len()
    }

    /// Check if the map contains the given key.
    pub fn contains_key(&self, key: &K) -> bool {
        self.map_contains_key(key)
    }

    /// Get a clone of the value associated with the given key if found.
    pub fn get_cloned(&self, key: &K) -> Option<V> {
        self.map
            .read()
            .expect("RwLock map poisoned")
            .get(key)
            .map(|entry| entry.value.clone())
    }

    /// Get the number of elements stored in the map.
    pub fn len(&self) -> usize {
        self.map_len()
    }

    /// Check if the map is empty.
    pub fn is_empty(&self) -> bool {
        self.map_len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ttl_hashmap() {
        let ttl = 3;
        let map = ExpiringHashMap::new(ttl);

        let ts = 1;
        map.insert(1, 1, ts);
        map.insert(2, 2, ts);

        assert_eq!(map.get_cloned(&1), Some(1));
        assert_eq!(map.get_cloned(&2), Some(2));
        assert_eq!(map.len(), 2);

        let ts = 2;
        map.insert(3, 3, ts);
        assert_eq!(map.get_cloned(&1), Some(1));
        assert_eq!(map.get_cloned(&2), Some(2));
        assert_eq!(map.get_cloned(&3), Some(3));
        assert_eq!(map.len(), 3);

        let ts = 3;
        map.insert(4, 4, ts);
        assert_eq!(map.get_cloned(&1), Some(1));
        assert_eq!(map.get_cloned(&2), Some(2));
        assert_eq!(map.get_cloned(&3), Some(3));
        assert_eq!(map.get_cloned(&4), Some(4));
        assert_eq!(map.len(), 4);

        let ts = 4;
        map.insert(5, 5, ts);
        assert_eq!(map.get_cloned(&1), None);
        assert_eq!(map.get_cloned(&2), None);
        assert_eq!(map.get_cloned(&3), Some(3));
        assert_eq!(map.get_cloned(&4), Some(4));
        assert_eq!(map.get_cloned(&5), Some(5));
        assert_eq!(map.len(), 3);

        map.insert(6, 6, ts);
        assert_eq!(map.get_cloned(&3), Some(3));
        assert_eq!(map.get_cloned(&4), Some(4));
        assert_eq!(map.get_cloned(&5), Some(5));
        assert_eq!(map.get_cloned(&6), Some(6));
        assert_eq!(map.len(), 4);

        let ts = 5;
        // Inserting 3 again should prevent that latest value to be removed
        // until the current ts (5) expires
        map.insert(3, 33, ts);
        assert_eq!(map.get_cloned(&3), Some(33));
        assert_eq!(map.get_cloned(&4), Some(4));
        assert_eq!(map.get_cloned(&5), Some(5));
        assert_eq!(map.get_cloned(&6), Some(6));
        assert_eq!(map.len(), 4);

        let ts = 6;
        map.insert(7, 7, ts);
        assert_eq!(map.get_cloned(&3), Some(33));
        assert_eq!(map.get_cloned(&4), None);
        assert_eq!(map.get_cloned(&5), Some(5));
        assert_eq!(map.get_cloned(&6), Some(6));
        assert_eq!(map.get_cloned(&7), Some(7));
        assert_eq!(map.len(), 4);

        let ts = 7;
        map.insert(8, 8, ts);
        assert_eq!(map.get_cloned(&3), Some(33));
        assert_eq!(map.get_cloned(&5), None);
        assert_eq!(map.get_cloned(&6), None);
        assert_eq!(map.get_cloned(&7), Some(7));
        assert_eq!(map.get_cloned(&8), Some(8));
        assert_eq!(map.len(), 3);

        let ts = 8;
        map.insert(9, 9, ts);
        assert_eq!(map.get_cloned(&3), None);
        assert_eq!(map.get_cloned(&7), Some(7));
        assert_eq!(map.get_cloned(&8), Some(8));
        assert_eq!(map.get_cloned(&9), Some(9));
        assert_eq!(map.len(), 3);

        let ts = 9;
        map.insert(9, 10, ts);
        assert_eq!(map.get_cloned(&7), None);
        assert_eq!(map.get_cloned(&8), Some(8));
        assert_eq!(map.get_cloned(&9), Some(10));
        assert_eq!(map.len(), 2);
    }
}
