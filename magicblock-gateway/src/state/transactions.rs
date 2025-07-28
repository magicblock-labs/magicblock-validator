use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use scc::{HashMap, Queue};
use solana_signature::Signature;

pub(crate) struct TransactionsCache {
    index: HashMap<Signature, bool>,
    queue: Queue<ExpiringSignature>,
}

struct ExpiringSignature {
    signature: Signature,
    genesis: Instant,
}

impl TransactionsCache {
    /// Initialize the cache, by allocating initial storage,
    /// and setting up an update listener loop
    pub fn init() -> Arc<Self> {
        Arc::new(Self {
            index: HashMap::with_capacity(1024 * 512),
            queue: Queue::default(),
        })
    }

    /// Push the new entry into the cache, evicting the expired ones in the process
    fn push(&self, signature: Signature, status: bool) {
        while let Ok(Some(expired)) = self.queue.pop_if(|e| e.expired()) {
            self.index.remove(&expired.signature);
        }
        self.queue.push(ExpiringSignature::new(signature));
        let _ = self.index.insert(signature, status);
    }

    /// Query the status of transaction from the cache
    pub fn get(&self, signature: &Signature) -> Option<bool> {
        self.index.read(signature, |_, &v| v)
    }
}

impl ExpiringSignature {
    #[inline]
    fn new(signature: Signature) -> Self {
        let genesis = Instant::now();
        Self { signature, genesis }
    }
    #[inline]
    fn expired(&self) -> bool {
        const CACHE_KEEP_ALIVE_TTL: Duration = Duration::from_secs(90);
        self.genesis.elapsed() >= CACHE_KEEP_ALIVE_TTL
    }
}
