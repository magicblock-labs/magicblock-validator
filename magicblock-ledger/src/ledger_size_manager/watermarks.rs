use log::*;
use std::collections::VecDeque;

use solana_sdk::clock::Slot;

use super::config::{ExistingLedgerState, ResizePercentage};

// -----------------
// Watermarks
// -----------------
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Watermark {
    /// The slot at which this watermark was captured
    pub(crate) slot: u64,
    /// Account mod ID at which this watermark was captured
    pub(crate) mod_id: u64,
    /// The size of the ledger when this watermark was captured
    pub(crate) size: u64,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct Watermarks {
    /// The watermarks captured
    pub(crate) marks: VecDeque<Watermark>,
    /// The maximum number of watermarks to keep
    count: u64,
    /// The targeted size difference for each watermark
    mark_size: u64,
    /// The maximum ledger size to maintain
    max_ledger_size: u64,
}

impl Watermarks {
    /// Creates a new set of watermarks based on the resize percentage and max ledger size.
    /// - * `percentage`: The resize percentage to use.
    /// - * `max_ledger_size`: The maximum size of the ledger to try to maintain.
    /// - * `ledger_state`: The current ledger state which is
    ///      only available during restart with an existing ledger
    pub(super) fn new(
        percentage: &ResizePercentage,
        max_ledger_size: u64,
        ledger_state: Option<ExistingLedgerState>,
    ) -> Self {
        let count = percentage.watermark_count();
        let mut marks = VecDeque::with_capacity(count as usize);
        let mark_size =
            (max_ledger_size * percentage.watermark_size_percent()) / 100;
        if let Some(ExistingLedgerState { size, slot, mod_id }) = ledger_state {
            // Since we don't know the actual ledger sizes at each slot we must assume
            // they were evenly distributed.
            let mark_size_delta = size / count;
            let mod_id_delta = mod_id / count;
            let slot_delta = slot / count;
            for i in 0..count {
                let size = (i + 1) * mark_size_delta;
                let mod_id = (i + 1) * mod_id_delta;
                let slot = (i + 1) * slot_delta;
                marks.push_back(Watermark { slot, mod_id, size });
            }
        }
        // In case we don't have an existing ledger state, we assume that the ledger size is
        // still zero and we won't need any fabricated watermarks.
        Watermarks {
            marks,
            count,
            mark_size,
            max_ledger_size,
        }
    }

    fn is_empty(&self) -> bool {
        self.marks.is_empty()
    }

    fn reached_max(&self, size: u64) -> bool {
        size >= self.max_ledger_size
    }

    pub(super) fn get_truncation_mark(
        &mut self,
        ledger_size: u64,
        slot: Slot,
        mod_id: u64,
    ) -> Option<Watermark> {
        if ledger_size == 0 {
            return None;
        }
        if self.reached_max(ledger_size) {
            debug!(
                "Ledger size {} reached maximum size {}, resizing...",
                ledger_size, self.max_ledger_size
            );
            self.consume_next()
        } else {
            self.update(slot, mod_id, ledger_size);
            None
        }
    }

    fn update(&mut self, slot: u64, mod_id: u64, size: u64) {
        // We try to record a watermark as close as possible (but below) the ideal
        // watermark cutoff size.
        let mark_idx = (size as f64 / self.mark_size as f64).ceil() as u64 - 1;
        if mark_idx < self.count {
            let watermark = Watermark { slot, mod_id, size };
            if let Some(mark) = self.marks.get_mut(mark_idx as usize) {
                *mark = watermark;
            } else {
                self.marks.push_back(watermark);
            }
        }
    }

    fn adjust_for_truncation(&mut self) {
        // The sizes recorded at specific slots need to be adjusted since we truncated
        // the slots before

        for mut mark in self.marks.iter_mut() {
            mark.size = mark.size.saturating_sub(self.mark_size);
        }
    }

    fn consume_next(&mut self) -> Option<Watermark> {
        self.marks.pop_front()
    }
}

#[cfg(test)]
mod tests {
    use test_tools_core::init_logger;

    use super::*;

    macro_rules! mark {
        ($slot:expr, $mod_id:expr, $size:expr) => {{
            Watermark {
                slot: $slot,
                mod_id: $mod_id,
                size: $size,
            }
        }};
        ($idx:expr, $size:expr) => {{
            mark!($idx, $idx, $size)
        }};
    }

    macro_rules! marks {
        ($($slot:expr, $mod_id:expr, $size:expr);+) => {{
            let mut marks = VecDeque::<Watermark>::new();
            $(
                marks.push_back(mark!($slot, $mod_id, $size));
            )+
            Watermarks {
                marks,
                count: 3,
                mark_size: 250,
                max_ledger_size: 1000,
            }
        }};
    }

    macro_rules! truncate_ledger {
        ($slot:expr, $mod_id:expr, $watermarks:ident, $mark:expr, $size:ident) => {{
            // These steps are usually performed in _actual_ ledger truncate method
            $size -= $mark.size;
            $watermarks.adjust_for_truncation();
            $watermarks.update($slot, $mod_id, $size);
            debug!(
                "Truncated ledger to size {} -> {:#?}",
                $size, $watermarks
            );
        }}
    }

    #[test]
    fn test_watermarks_new_ledger() {
        init_logger!();

        let percentage = ResizePercentage::Large;
        const MAX_SIZE: u64 = 1_000;
        const STEP_SIZE: u64 = MAX_SIZE / 20;
        let mut watermarks = Watermarks::new(&percentage, MAX_SIZE, None);

        // 1. Go up to right below the ledger size
        let mut size = 0;
        for i in 0..19 {
            size += STEP_SIZE;
            let mark = watermarks.get_truncation_mark(size, i, i);
            assert!(
                mark.is_none(),
                "Expected no truncation mark at size {}",
                size
            );
        }

        assert_eq!(watermarks, marks!(4, 4, 250; 9, 9, 500; 14, 14, 750));

        // 2. Hit ledger max size
        size += STEP_SIZE;
        let mark = watermarks.get_truncation_mark(size, 20, 20);
        assert_eq!(watermarks, marks!(9, 9, 500; 14, 14, 750));
        assert_eq!(mark, Some(mark!(4, 4, 250)));

        truncate_ledger!(20, 20, watermarks, mark.unwrap(), size);
        assert_eq!(watermarks, marks!(9, 9, 250; 14, 14, 500; 20, 20, 750));

        // 3. Go up to right below the next truncation mark (also ledger max size)
        for i in 21..=24 {
            size += STEP_SIZE;
            let mark = watermarks.get_truncation_mark(size, i, i);
            assert!(
                mark.is_none(),
                "Expected no truncation mark at size {}",
                size
            );
        }
        assert_eq!(watermarks, marks!(9, 9, 250; 14, 14, 500; 20, 20, 750));

        // 4. Hit next truncation mark (also ledger max size)
        size += STEP_SIZE;
        let mark = watermarks.get_truncation_mark(size, 25, 25);
        assert_eq!(mark, Some(mark!(9, 9, 250)));

        truncate_ledger!(25, 25, watermarks, mark.unwrap(), size);
        assert_eq!(watermarks, marks!(14, 14, 250; 20, 20, 500; 25, 25, 750));

        // 5. Go past 3 truncation marks
        for i in 26..=40 {
            size += STEP_SIZE;
            let mark = watermarks.get_truncation_mark(size, i, i);
            if mark.is_some() {
                truncate_ledger!(i, i, watermarks, mark.unwrap(), size);
            }
        }

        assert_eq!(watermarks, marks!(30, 30, 250; 35, 35, 500; 40, 40, 750));
    }

    #[test]
    fn test_watermarks_existing_ledger() {
        init_logger!();

        let percentage = ResizePercentage::Large;
        const MAX_SIZE: u64 = 1_000;
        const STEP_SIZE: u64 = MAX_SIZE / 20;
        let ledger_state = ExistingLedgerState {
            // NOTE: that the watermarks will always be adjusted to have the size
            // lower than the max size before we start using the watermark strategy.
            // See [`ensure_initial_max_ledger_size`].
            size: 900,
            slot: 150,
            mod_id: 150,
        };
        let watermarks =
            Watermarks::new(&percentage, MAX_SIZE, Some(ledger_state));

        // Initial watermarks should be based on the existing ledger state
        assert_eq!(
            watermarks,
            marks!(50, 50, 300; 100, 100, 600; 150, 150, 900)
        );
    }
}
