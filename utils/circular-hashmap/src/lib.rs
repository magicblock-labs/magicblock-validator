use std::collections::HashMap;

/// Wrapper around a [HashMap] that ensures that only a maximum number of elements are stored.
/// When the map is full and a new element is added the oldest element is removed.
#[derive(Debug, Clone)]
pub struct CircularHashMap<K: PartialEq + Eq + std::hash::Hash + Clone, V> {
    map: HashMap<K, V>,
    vec: Vec<K>,
    next_vec_index: usize,
    max_size: usize,
}

impl<K: PartialEq + Eq + std::hash::Hash + Clone, V> CircularHashMap<K, V> {
    /// Creates a new CircularHashMap with the given max size.
    pub fn new(max_size: usize) -> Self {
        CircularHashMap {
            map: HashMap::new(),
            vec: Vec::with_capacity(max_size),
            next_vec_index: 0,
            max_size,
        }
    }

    /// Insert a new key-value pair into the map.
    /// If the map is full the oldest element is removed.
    pub fn insert(&mut self, key: K, value: V) {
        // If the map is full we remove the oldest element
        if self.vec.len() == self.max_size {
            let old_key = self.vec.get(self.next_vec_index).unwrap();
            self.map.remove(old_key);
        } else {
            self.vec.push(key.clone());
        }
        self.map.insert(key, value);
        self.next_vec_index = (self.next_vec_index + 1) % self.max_size;
    }

    /// Check if the map contains the given key.
    pub fn contains_key(&self, key: &K) -> bool {
        self.map.contains_key(key)
    }

    /// Get a reference to the value associated with the given key.
    pub fn get(&self, key: &K) -> Option<&V> {
        self.map.get(key)
    }

    /// Get the number of elements stored in the map.
    pub fn len(&self) -> usize {
        self.map.len()
    }

    /// Check if the map is empty.
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
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
    fn test_circular_hashmap() {
        let mut map = CircularHashMap::new(3);

        map.insert(1, 1);
        assert_eq!(map.get(&1), Some(&1));

        map.insert(2, 2);
        assert_eq!(map.get(&2), Some(&2));

        map.insert(3, 3);
        assert_eq!(map.get(&3), Some(&3));

        map.insert(4, 4);
        assert!(!map.contains_key(&1));
        assert_eq!(map.get(&1), None);
        assert!(map.contains_key(&2));
        assert_eq!(map.get(&2), Some(&2));
        assert!(map.contains_key(&3));
        assert_eq!(map.get(&3), Some(&3));
        assert!(map.contains_key(&4));
        assert_eq!(map.get(&4), Some(&4));

        map.insert(5, 5);
        assert_eq!(map.get(&1), None);
        assert_eq!(map.get(&2), None);
        assert_eq!(map.get(&3), Some(&3));
        assert_eq!(map.get(&4), Some(&4));
        assert_eq!(map.get(&5), Some(&5));

        assert_eq!(map.len(), 3);
    }
}
