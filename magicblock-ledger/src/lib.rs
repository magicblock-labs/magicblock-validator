use std::sync::Arc;

use arc_swap::{ArcSwapAny, Guard};
pub use database::{
    meta::PerfSample, options::BLOCKSTORE_DIRECTORY_ROCKS_LEVEL,
};
use magicblock_core::traits::LatestBlockProvider;
use solana_clock::Clock;
use solana_hash::Hash;
pub use store::api::{Ledger, SignatureInfosForAddress};
use tokio::sync::broadcast;

#[derive(Default, Clone)]
pub struct LatestBlockInner {
    pub slot: u64,
    pub blockhash: Hash,
    pub clock: Clock,
    pub timestamp_millis: i64,
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
    notifier: broadcast::Sender<LatestBlockInner>,
}

impl LatestBlockInner {
    /// Creates a block from a whole-second timestamp. The millisecond timestamp
    /// is that value with a zero sub-second component.
    pub fn new(slot: u64, blockhash: Hash, timestamp: i64) -> Self {
        Self::new_with_millis(slot, blockhash, timestamp * 1000)
    }

    /// Creates a block from a millisecond-accurate timestamp, deriving the
    /// whole-second `Clock::unix_timestamp` from it.
    pub fn new_with_millis(
        slot: u64,
        blockhash: Hash,
        timestamp_millis: i64,
    ) -> Self {
        let clock = Clock {
            slot: slot + 1,
            unix_timestamp: timestamp_millis.div_euclid(1000),
            ..Default::default()
        };
        Self {
            slot,
            blockhash,
            clock,
            timestamp_millis,
        }
    }
}

impl Default for LatestBlock {
    fn default() -> Self {
        let (notifier, _) = broadcast::channel(32);
        let inner = Default::default();
        Self { inner, notifier }
    }
}

impl LatestBlock {
    /// Atomically loads a snapshot of the latest block information.
    /// This provides a high-performance, lock-free read.
    pub fn load(&self) -> Guard<Arc<LatestBlockInner>> {
        self.inner.load()
    }

    /// Atomically updates the latest block information and notifies all subscribers.
    /// This is the "writer" method for the single-writer, multi-reader pattern.
    pub fn store(&self, block: LatestBlockInner) {
        self.inner.store(block.clone().into());
        // Broadcast the update. It's okay if there are no active listeners.
        let _ = self.notifier.send(block);
    }

    /// Creates a new receiver to listen for block updates.
    /// Each receiver created via this method will be notified when `store` is called.
    /// This allows multiple components to react to new blocks concurrently.
    pub fn subscribe(&self) -> broadcast::Receiver<LatestBlockInner> {
        self.notifier.subscribe()
    }
}

impl LatestBlockProvider for LatestBlock {
    fn slot(&self) -> u64 {
        self.inner.load().slot
    }

    fn blockhash(&self) -> Hash {
        self.inner.load().blockhash
    }

    fn clock(&self) -> Clock {
        self.inner.load().clock.clone()
    }
}

pub mod blockstore_processor;

mod database;
pub mod errors;
pub mod ledger_truncator;
mod metrics;
mod store;
