use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use magicblock_metrics::metrics;

/// Wrapper around Arc<AtomicU64> that automatically captures metrics
/// when the chain slot is updated.
#[derive(Clone, Debug)]
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

    /// The maximum amount of slots we expect to pass from the time
    /// a subscription is requested until the point when it is
    /// activated. ~10 secs
    pub const MAX_SLOTS_SUB_ACTIVATION: u64 = 25;

    /// Computes a `from_slot` for backfilling based on the current
    /// chain slot.
    pub fn compute_from_slot(&self) -> u64 {
        self.load().saturating_sub(Self::MAX_SLOTS_SUB_ACTIVATION)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, atomic::AtomicU64};

    use super::*;

    fn make(slot: u64) -> ChainSlot {
        ChainSlot::new(Arc::new(AtomicU64::new(slot)))
    }

    #[test]
    fn returns_zero_when_slot_is_zero() {
        let cs = make(0);
        assert_eq!(cs.compute_from_slot(), 0);
    }

    #[test]
    fn returns_subtracted_slot_when_slot_is_nonzero() {
        let cs = make(1000);
        assert_eq!(
            cs.compute_from_slot(),
            1000 - ChainSlot::MAX_SLOTS_SUB_ACTIVATION,
        );
    }

    #[test]
    fn saturates_at_zero_when_slot_below_window() {
        let cs = make(1);
        assert_eq!(cs.compute_from_slot(), 0);
    }
}
