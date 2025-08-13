use std::sync::Arc;

use arc_swap::{ArcSwapAny, Guard};
pub use database::meta::PerfSample;
use solana_sdk::{clock::Clock, hash::Hash};
pub use store::api::{Ledger, SignatureInfosForAddress};
use tokio::sync::Notify;

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
#[derive(Clone, Default)]
pub struct LatestBlock {
    /// Atomically swappable block data, the reference can be safely
    /// accessed by multiple threads, even if another threads swaps
    /// the value from under them. As long as there're some readers,
    /// the reference will be kept alive by arc swap, while the new
    /// readers automatically get access to the latest version of the block
    inner: Arc<ArcSwapAny<Arc<LatestBlockInner>>>,
    /// Notification mechanism to signal that the block has been modified
    notifier: Arc<Notify>,
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

impl LatestBlock {
    pub fn load(&self) -> Guard<Arc<LatestBlockInner>> {
        self.inner.load()
    }

    pub fn store(&self, slot: u64, blockhash: Hash, timestamp: i64) {
        let block = LatestBlockInner::new(slot, blockhash, timestamp);
        self.inner.store(block.into());
        self.notifier.notify_waiters();
    }

    pub async fn changed(&self) {
        self.notifier.notified().await
    }
}

pub mod blockstore_processor;
mod conversions;
mod database;
pub mod errors;
pub mod ledger_truncator;
mod metrics;
mod store;
