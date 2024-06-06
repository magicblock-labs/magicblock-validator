use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, RwLock,
    },
};

#[derive(Debug, Clone)]
pub struct CountedEntry<V: Clone> {
    value: V,
    count: usize,
}

// -----------------
// SharedMap
// -----------------
/// Shared access to a [HashMap] wrapped in a [RwLock] and [Arc], but only
/// exposing query methods.
/// Consider it a limited interface for the [CircularHashMap].
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
// CircularHashMap
// -----------------
/// Wrapper around a [HashMap] that ensures that only a maximum number of elements are stored.
/// When the map is full and a new element is added the oldest element is removed.
#[derive(Debug)]
pub struct CircularHashMap<K, V>
where
    K: PartialEq + Eq + std::hash::Hash + Clone,
    V: Clone,
{
    map: Arc<RwLock<HashMap<K, CountedEntry<V>>>>,
    vec: Arc<RwLock<Vec<K>>>,
    next_vec_index: AtomicUsize,
    max_size: usize,
}

impl<K, V> CircularHashMap<K, V>
where
    K: PartialEq + Eq + std::hash::Hash + Clone,
    V: Clone,
{
    /// Creates a new CircularHashMap with the given max size.
    pub fn new(max_size: usize) -> Self {
        CircularHashMap {
            map: Arc::<RwLock<HashMap<K, CountedEntry<V>>>>::default(),
            vec: Arc::new(RwLock::new(Vec::with_capacity(max_size))),
            next_vec_index: AtomicUsize::default(),
            max_size,
        }
    }

    /// Insert a new key-value pair into the map.
    /// If the map is full the oldest element is removed.
    pub fn insert(&self, key: K, value: V) {
        // While inserting a new entry we ensure that our cache size doesn't grow larger
        // than the max size.
        // Thus we evict entries inserted before by circling in our ring buffer.
        // However if the same key is inserted multiple times we need to only remove it from
        // the map once we evict the latest entry of that key from our ring buffer.
        // To that end we count how many times the entry was inserted and decrease that each time
        // we find it in our ring buffer. Only once that count goes to zero do we evict it from the
        // map.

        // 1. Insert the new entry
        let entry = if let Some(mut entry) = self.map_remove(&key) {
            entry.count += 1;
            entry.value = value;
            entry
        } else {
            CountedEntry { value, count: 1 }
        };
        self.map_insert(key.clone(), entry);

        // 2. Move the index in our ring buffer
        let vec_index = self
            .next_vec_index
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |x| {
                Some((x + 1) % self.max_size)
            })
            .unwrap();

        // 3. Use the ring buffer to tell us if we need to remove an existing entry
        if self.vec_len() >= self.max_size {
            let old_key = self.vec_replace(vec_index, key.clone());
            self.map_decrease_count_and_maybe_remove(&old_key);
        } else {
            self.vec_push(key.clone());
        }
    }

    pub fn shared_map(&self) -> SharedMap<K, V> {
        SharedMap(self.map.clone())
    }

    fn vec_len(&self) -> usize {
        self.vec.read().expect("RwLock vec poisoned").len()
    }

    fn vec_push(&self, key: K) {
        self.vec.write().expect("RwLock vec poisoned").push(key);
    }

    fn vec_replace(&self, index: usize, key: K) -> K {
        std::mem::replace(
            &mut self.vec.write().expect("RwLock vec poisoned")[index],
            key,
        )
    }

    fn map_remove(&self, key: &K) -> Option<CountedEntry<V>> {
        self.map.write().expect("RwLock map poisoned").remove(key)
    }

    fn map_decrease_count_and_maybe_remove(&self, key: &K) {
        // If a particular entry was updated multiple times it is present in our ring buffer
        // at multiple indexes. We want to remove it only once we find the last of those.
        let remove = if let Some(entry) =
            self.map.write().expect("RwLock map poisoned").get_mut(key)
        {
            entry.count -= 1;
            entry.count == 0
        } else {
            false
        };

        // This happens rarely for accounts that don't see updates for a long time
        if remove {
            self.map_remove(key);
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

    /// Get the max size of the map.
    pub fn max_size(&self) -> usize {
        self.max_size
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_circular_hashmap_inserting_different_keys() {
        let map = CircularHashMap::new(3);

        map.insert(1, 1);
        assert_eq!(map.get_cloned(&1), Some(1));

        map.insert(2, 2);
        assert_eq!(map.get_cloned(&2), Some(2));

        map.insert(3, 3);
        assert_eq!(map.get_cloned(&3), Some(3));

        map.insert(4, 4);
        assert!(!map.contains_key(&1));
        assert_eq!(map.get_cloned(&1), None);
        assert!(map.contains_key(&2));
        assert_eq!(map.get_cloned(&2), Some(2));
        assert!(map.contains_key(&3));
        assert_eq!(map.get_cloned(&3), Some(3));
        assert!(map.contains_key(&4));
        assert_eq!(map.get_cloned(&4), Some(4));

        map.insert(5, 5);
        assert_eq!(map.get_cloned(&1), None);
        assert_eq!(map.get_cloned(&2), None);
        assert_eq!(map.get_cloned(&3), Some(3));
        assert_eq!(map.get_cloned(&4), Some(4));
        assert_eq!(map.get_cloned(&5), Some(5));

        assert_eq!(map.len(), 3);

        map.insert(6, 6);
        assert_eq!(map.get_cloned(&3), None);
        assert_eq!(map.get_cloned(&4), Some(4));
        assert_eq!(map.get_cloned(&5), Some(5));
        assert_eq!(map.get_cloned(&6), Some(6));

        assert_eq!(map.len(), 3);

        map.insert(7, 7);
        assert_eq!(map.get_cloned(&4), None);
        assert_eq!(map.get_cloned(&5), Some(5));
        assert_eq!(map.get_cloned(&6), Some(6));
        assert_eq!(map.get_cloned(&7), Some(7));
    }

    #[test]
    fn test_circular_hashmap_inserting_same_key() {
        let map = CircularHashMap::new(3);

        map.insert(1, 1);
        assert_eq!(map.get_cloned(&1), Some(1));
        assert_eq!(map.len(), 1);

        map.insert(1, 2);
        assert_eq!(map.get_cloned(&1), Some(2));
        assert_eq!(map.len(), 1);

        map.insert(1, 3);
        assert_eq!(map.get_cloned(&1), Some(3));
        assert_eq!(map.len(), 1);

        map.insert(2, 2);
        assert_eq!(map.len(), 2);
        assert_eq!(map.get_cloned(&1), Some(3));
        assert_eq!(map.get_cloned(&2), Some(2));

        map.insert(3, 3);
        assert_eq!(map.len(), 3);
        assert_eq!(map.get_cloned(&1), Some(3));
        assert_eq!(map.get_cloned(&2), Some(2));
        assert_eq!(map.get_cloned(&3), Some(3));

        map.insert(1, 4);
        assert_eq!(map.get_cloned(&1), Some(4));
        assert_eq!(map.len(), 3);

        // The fourth key causes another one to be removed
        map.insert(4, 4);
        assert_eq!(map.len(), 3);
        // 1 was updated last so it should still be there
        assert_eq!(map.get_cloned(&1), Some(4));
        // 2 was updated before 3 was inserted and before 1 was updated
        assert!(!map.contains_key(&2));
        assert_eq!(map.get_cloned(&3), Some(3));
        assert_eq!(map.get_cloned(&4), Some(4));
    }
}
