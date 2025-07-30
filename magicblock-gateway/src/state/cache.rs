use std::{
    hash::Hash,
    time::{Duration, Instant},
};

pub(crate) struct ExpiringCache<K, V> {
    index: scc::HashMap<K, V>,
    queue: scc::Queue<ExpiringRecord<K>>,
}

struct ExpiringRecord<K> {
    key: K,
    genesis: Instant,
}

impl<K: Hash + Eq + Copy + 'static, V: Clone> ExpiringCache<K, V> {
    /// Initialize the cache, by allocating initial storage,
    /// and setting up an update listener loop
    pub(crate) fn new(capacity: usize) -> Self {
        Self {
            index: scc::HashMap::with_capacity(capacity),
            queue: scc::Queue::default(),
        }
    }

    /// Push the new entry into the cache, evicting the expired ones in the process
    pub(crate) fn push(&self, key: K, value: V) {
        while let Ok(Some(expired)) = self.queue.pop_if(|e| e.expired()) {
            self.index.remove(&expired.key);
        }
        self.queue.push(ExpiringRecord::new(key));
        let _ = self.index.insert(key, value);
    }

    /// Query the status of transaction from the cache
    pub(crate) fn get(&self, key: &K) -> Option<V> {
        self.index.read(key, |_, v| v.clone())
    }

    /// Query the status of transaction from the cache
    pub(crate) fn contains(&self, key: &K) -> bool {
        self.index.contains(key)
    }
}

impl<K> ExpiringRecord<K> {
    #[inline]
    fn new(key: K) -> Self {
        let genesis = Instant::now();
        Self { key, genesis }
    }
    #[inline]
    fn expired(&self) -> bool {
        const CACHE_KEEP_ALIVE_TTL: Duration = Duration::from_secs(90);
        self.genesis.elapsed() >= CACHE_KEEP_ALIVE_TTL
    }
}
