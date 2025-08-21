use std::sync::Arc;

use arc_swap::{ArcSwapAny, Guard};
pub use database::meta::PerfSample;
use solana_sdk::{clock::Clock, hash::Hash};
pub use store::api::{Ledger, SignatureInfosForAddress};
use tokio::sync::broadcast;

#[derive(Default)]
pub struct LatestBlockInner {
    pub slot: u64,
    pub blockhash: Hash,
    pub clock: Clock,
}

/// Atomically updated, shared, latest block information
/// The instances of this type can be used by various components
/// of the validator to cheaply retrieve the latest block data,
/// without relying on expensive ledger operations. It's always
/// kept in sync with the ledger by the ledger itself
#[derive(Clone)]
pub struct LatestBlock {
    /// Atomically swappable block data, the reference can be safely
    /// accessed by multiple threads, even if another threads swaps
    /// the value from under them. As long as there're some readers,
    /// the reference will be kept alive by arc swap, while the new
    /// readers automatically get access to the latest version of the block
    inner: Arc<ArcSwapAny<Arc<LatestBlockInner>>>,
    /// Notification mechanism to signal that the block has been modified,
    /// the actual state is not sent via channel, as it can be accessed any
    /// time with `load` method, only the fact of production is communicated
    notifier: broadcast::Sender<()>,
}

impl LatestBlockInner {
    fn new(slot: u64, blockhash: Hash, timestamp: i64) -> Self {
        let clock = Clock {
            slot,
            unix_timestamp: timestamp,
            ..Default::default()
        };
        Self {
            slot,
            blockhash,
            clock,
        }
    }
}

impl Default for LatestBlock {
    fn default() -> Self {
        // 1 is just enough number of notifications to keep around, in order to cover
        // cases when a subscriber might not be listening when broadcast is triggered
        let (notifier, _) = broadcast::channel(1);
        let inner = Default::default();
        Self { inner, notifier }
    }
}

impl LatestBlock {
    pub fn load(&self) -> Guard<Arc<LatestBlockInner>> {
        self.inner.load()
    }

    pub fn store(&self, slot: u64, blockhash: Hash, timestamp: i64) {
        let block = LatestBlockInner::new(slot, blockhash, timestamp);
        self.inner.store(block.into());
        // we don't care if there're active listeners
        let _ = self.notifier.send(());
    }

    pub fn subscribe(&self) -> broadcast::Receiver<()> {
        self.notifier.subscribe()
    }
}

pub mod blockstore_processor;
mod conversions;
mod database;
pub mod errors;
pub mod ledger_truncator;
mod metrics;
mod store;
