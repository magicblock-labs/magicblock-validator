use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};

use magicblock_metrics::metrics;

/// Wrapper around Arc<AtomicU64> that automatically captures metrics
/// when the chain slot is updated.
#[derive(Clone)]
pub struct ChainSlot {
    slot: Arc<AtomicU64>,
}

impl ChainSlot {
    pub fn new(slot: Arc<AtomicU64>) -> Self {
        Self { slot }
    }

    /// Updates the chain slot to the maximum of the current and new value.
    ///
    /// Uses `fetch_max()` to ensure monotonically increasing values and
    /// captures metrics only if the value actually changed.
    pub fn update(&self, new_slot: u64) {
        let prev_slot = self.slot.fetch_max(new_slot, Ordering::Relaxed);
        if new_slot > prev_slot {
            metrics::set_chain_slot(new_slot);
        }
    }

    /// Loads the current chain slot value.
    pub fn load(&self) -> u64 {
        self.slot.load(Ordering::Relaxed)
    }
}
