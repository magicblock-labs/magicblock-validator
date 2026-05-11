use std::{
    collections::{HashMap, VecDeque},
    hash::Hash,
    time::{Duration, Instant},
};

use parking_lot::Mutex;

type CacheInner<K, V> = (HashMap<K, V>, VecDeque<ExpiringRecord<K>>);

/// A thread-safe, expiring cache with lazy eviction.
///
/// This cache stores key-value pairs for a specified duration (time-to-live).
///
/// Eviction of expired entries is performed **lazily**: the cache is only cleaned
/// when a new element is inserted via the [`push`] method. There is no background
/// thread for cleanup.
pub(crate) struct ExpiringCache<K, V> {
    inner: Mutex<CacheInner<K, V>>,
    /// The time-to-live for each entry from its moment of creation.
    ttl: Duration,
}

/// An internal record used to track the creation time of a cache key.
struct ExpiringRecord<K> {
    /// The key of the cached entry.
    key: K,
    /// The timestamp captured when the entry was first created.
    genesis: Instant,
}

impl<K: Hash + Eq + Clone, V: Clone> ExpiringCache<K, V> {
    /// Creates a new `ExpiringCache` with a specified time-to-live (TTL) for all entries.
    pub(crate) fn new(ttl: Duration) -> Self {
        Self {
            inner: Mutex::new((HashMap::new(), VecDeque::new())),
            ttl,
        }
    }

    /// Inserts a key-value pair into the cache and evicts any expired entries.
    ///
    /// Before insertion, this method performs a lazy cleanup by removing all entries
    /// from the head of the queue that have exceeded their TTL.
    ///
    /// If the key already exists, its value is updated.
    /// **Note:** The entry's lifetime is **not** renewed upon
    /// update; it retains its original creation timestamp.
    ///
    /// # Returns
    ///
    /// Returns `true` if the key was newly inserted, or `false` if the key
    /// already existed and its value was updated.
    pub(crate) fn push(&self, key: K, value: V) -> bool {
        let mut guard = self.inner.lock();
        let (index, queue) = &mut *guard;

        // Lazily evict expired entries from the front of the queue.
        while let Some(record) = queue.pop_front_if(|e| e.expired(self.ttl)) {
            index.remove(&record.key);
        }

        // Insert or update the key-value pair.
        let is_new = index.insert(key.clone(), value).is_none();
        // If the key is new, add a corresponding record to the expiration queue.
        if is_new {
            queue.push_back(ExpiringRecord::new(key));
        }
        is_new
    }

    /// Retrieves a clone of the value associated with the given key, if it exists.
    pub(crate) fn get(&self, key: &K) -> Option<V> {
        self.inner.lock().0.get(key).cloned()
    }

    /// Checks if the cache contains a value for the specified key.
    pub(crate) fn contains(&self, key: &K) -> bool {
        self.inner.lock().0.contains_key(key)
    }
}

impl<K> ExpiringRecord<K> {
    /// Creates a new record, capturing the current time as its genesis timestamp.
    #[inline]
    fn new(key: K) -> Self {
        Self {
            key,
            genesis: Instant::now(),
        }
    }

    /// Returns `true` if the time elapsed since creation is greater than or equal to the TTL.
    #[inline]
    fn expired(&self, ttl: Duration) -> bool {
        self.genesis.elapsed() >= ttl
    }
}
