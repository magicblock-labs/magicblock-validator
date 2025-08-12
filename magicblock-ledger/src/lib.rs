use std::sync::Arc;

use arc_swap::{ArcSwapAny, Guard};
pub use database::meta::PerfSample;
use solana_sdk::{clock::Clock, hash::Hash};
pub use store::api::{Ledger, SignatureInfosForAddress};
use tokio::sync::Notify;

pub struct LatestBlockInner {
    pub slot: u64,
    pub blockhash: Hash,
    pub clock: Clock,
}

/// Atomically updated, shared, latest block information
#[derive(Clone)]
pub struct LatestBlock {
    inner: Arc<ArcSwapAny<Arc<LatestBlockInner>>>,
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
    pub fn new(slot: u64, blockhash: Hash, timestamp: i64) -> Self {
        let block = LatestBlockInner::new(slot, blockhash, timestamp);
        let notifier = Arc::default();
        Self {
            inner: Arc::new(ArcSwapAny::new(block.into())),
            notifier,
        }
    }

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
