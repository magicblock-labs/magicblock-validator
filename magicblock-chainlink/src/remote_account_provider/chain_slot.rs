use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
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
    ///
    /// Returns `None` while the slot is still `0`, i.e. before any
    /// real slot update has been observed. In that case the caller
    /// must treat backfill as temporarily unavailable for this
    /// subscription instead of sending `from_slot = 0`.
    pub fn compute_from_slot(&self) -> Option<u64> {
        let current = self.load();
        if current == 0 {
            return None;
        }
        Some(current.saturating_sub(Self::MAX_SLOTS_SUB_ACTIVATION))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{atomic::AtomicU64, Arc};

    use super::*;

    fn make(slot: u64) -> ChainSlot {
        ChainSlot::new(Arc::new(AtomicU64::new(slot)))
    }

    #[test]
    fn returns_none_when_slot_is_zero() {
        let cs = make(0);
        assert_eq!(cs.compute_from_slot(), None);
    }

    #[test]
    fn returns_subtracted_slot_when_slot_is_nonzero() {
        let cs = make(1000);
        assert_eq!(
            cs.compute_from_slot(),
            Some(1000 - ChainSlot::MAX_SLOTS_SUB_ACTIVATION),
        );
    }

    #[test]
    fn saturates_at_zero_when_slot_below_window() {
        let cs = make(1);
        assert_eq!(cs.compute_from_slot(), Some(0));
    }
}
