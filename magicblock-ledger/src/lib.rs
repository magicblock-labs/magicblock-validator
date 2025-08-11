use std::sync::Arc;

use arc_swap::{ArcSwapAny, Guard};
pub use database::meta::PerfSample;
use solana_sdk::hash::Hash;
pub use store::api::{Ledger, SignatureInfosForAddress};
use tokio::sync::Notify;

pub struct LatestBlockInner {
    pub slot: u64,
    pub blockhash: Hash,
}

/// Atomically updated, shared, latest block information
#[derive(Clone)]
pub struct LatestBlock {
    inner: Arc<ArcSwapAny<Arc<LatestBlockInner>>>,
    notifier: Arc<Notify>,
}

impl LatestBlock {
    pub fn new(slot: u64, blockhash: Hash) -> Self {
        let block = LatestBlockInner { slot, blockhash };
        let notifier = Arc::default();
        Self {
            inner: Arc::new(ArcSwapAny::new(block.into())),
            notifier,
        }
    }

    pub fn load(&self) -> Guard<Arc<LatestBlockInner>> {
        self.inner.load()
    }

    pub fn store(&self, slot: u64, blockhash: Hash) {
        let block = LatestBlockInner { slot, blockhash };
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
