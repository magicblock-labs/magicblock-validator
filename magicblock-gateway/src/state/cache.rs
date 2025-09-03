use std::{
    hash::Hash,
    time::{Duration, Instant},
};

/// A thread-safe, expiring cache with lazy eviction.
///
/// This cache stores key-value pairs for a specified duration (time-to-live).
/// It is designed for concurrent access using lock-free data structures.
///
/// Eviction of expired entries is performed **lazily**: the cache is only cleaned
/// when a new element is inserted via the [`push`] method. There is no background
/// thread for cleanup.
pub(crate) struct ExpiringCache<K, V> {
    /// A concurrent hash map providing fast, thread-safe key-value lookups.
    index: scc::HashMap<K, V>,
    /// A concurrent FIFO queue tracking the creation order of entries.
    ///
    /// This allows for efficient, ordered checks to find and evict the oldest
    /// (and therefore most likely to be expired) entries.
    queue: scc::Queue<ExpiringRecord<K>>,
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

impl<K: Hash + Eq + Copy + 'static, V: Clone> ExpiringCache<K, V> {
    /// Creates a new `ExpiringCache` with a specified time-to-live (TTL) for all entries.
    pub(crate) fn new(ttl: Duration) -> Self {
        Self {
            index: scc::HashMap::default(),
            queue: scc::Queue::default(),
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
        // Lazily evict expired entries from the front of the queue.
        while let Ok(Some(expired)) = self.queue.pop_if(|e| e.expired(self.ttl))
        {
            self.index.remove(&expired.key);
        }

        // Insert or update the key-value pair.
        let is_new = self.index.upsert(key, value).is_none();

        // If the key is new, add a corresponding record to the expiration queue.
        if is_new {
            self.queue.push(ExpiringRecord::new(key));
        }
        is_new
    }

    /// Retrieves a clone of the value associated with the given key, if it exists.
    pub(crate) fn get(&self, key: &K) -> Option<V> {
        self.index.read(key, |_, v| v.clone())
    }

    /// Checks if the cache contains a value for the specified key.
    pub(crate) fn contains(&self, key: &K) -> bool {
        self.index.contains(key)
    }
}

impl<K> ExpiringRecord<K> {
    /// Creates a new record, capturing the current time as its genesis timestamp.
    #[inline]
    fn new(key: K) -> Self {
        let genesis = Instant::now();
        Self { key, genesis }
    }

    /// Returns `true` if the time elapsed since creation is greater than or equal to the TTL.
    #[inline]
    fn expired(&self, ttl: Duration) -> bool {
        self.genesis.elapsed() >= ttl
    }
}
