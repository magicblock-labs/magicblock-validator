use std::collections::VecDeque;

use log::*;
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
    /// NOTE: this tells us up to where to clean account mods, however
    /// the size of that column is very small and thus we don't clean it yet
    pub(crate) mod_id: u64,
    /// The size delta relative to the previous captured watermark
    /// This tells us how much size we gain by removing all slots
    /// added since the last watermark.
    pub(crate) size_delta: u64,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct Watermarks {
    /// The watermarks captured
    pub(crate) marks: VecDeque<Watermark>,
    /// The size of the ledger when the last watermark was captured.
    /// NOTE: this is updated by the watermarks client, NOT by the watermark
    ///        implementation itself due to the fact that it isn't aware of the
    ///        true ledger size at time of capture or after it was truncated
    pub(crate) size_at_last_capture: u64,
    /// The targeted size difference for each watermark
    pub(crate) mark_size: u64,
    /// The maximum ledger size to maintain
    pub(crate) max_ledger_size: u64,
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
        let initial_size =
            if let Some(ExistingLedgerState { size, slot, mod_id }) =
                ledger_state
            {
                // Since we don't know the actual ledger sizes at each slot we must assume
                // they were evenly distributed.
                let mark_size_delta =
                    (size as f64 / count as f64).round() as u64;
                let mod_id_delta = mod_id / count;
                let slot_delta = (slot as f64 / count as f64).round() as u64;

                for i in 1..=count {
                    let mod_id = i * mod_id_delta;
                    let slot = i * slot_delta;
                    let mark = Watermark {
                        slot,
                        mod_id,
                        size_delta: mark_size_delta,
                    };
                    marks.push_back(mark);
                }
                size
            } else {
                0
            };
        Watermarks {
            marks,
            mark_size,
            max_ledger_size,
            size_at_last_capture: initial_size,
        }
    }

    fn is_empty(&self) -> bool {
        self.marks.is_empty()
    }

    fn reached_max(&self, size: u64) -> bool {
        size >= self.max_ledger_size
    }

    /// Returns a tuple containing two items:
    /// - an optional watermark specifying the slot to truncate to and
    ///   an indicator of how much size should be recuperated as a result
    /// - a boolean indicating if a new watermark was captured.
    ///
    /// NOTE: if a new watermark is captured the caller needs to update the
    /// [Watermarks::size_at_last_capture] with the current ledger size or the size
    /// resulting from the truncation applied when a watermark is returned.
    pub(super) fn get_truncation_mark(
        &mut self,
        ledger_size: u64,
        slot: Slot,
        mod_id: u64,
    ) -> (Option<Watermark>, bool) {
        if ledger_size == 0 {
            return (None, false);
        }
        let mark = if self.reached_max(ledger_size) {
            debug!(
                "Ledger size {} exceeded maximum size {}, resizing...",
                ledger_size, self.max_ledger_size
            );
            self.consume_next()
        } else {
            None
        };
        let captured = self.update(slot, mod_id, ledger_size);
        (mark, captured)
    }

    pub fn push_front(&mut self, mark: Watermark) {
        self.marks.push_front(mark);
    }

    fn update(&mut self, slot: u64, mod_id: u64, size: u64) -> bool {
        let size_delta = size.saturating_sub(self.size_at_last_capture);
        if size_delta >= self.mark_size {
            let mark = Watermark {
                slot,
                mod_id,
                size_delta,
            };
            self.marks.push_back(mark);
            true
        } else {
            false
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
        ($slot:expr, $mod_id:expr, $size_delta:expr) => {{
            Watermark {
                slot: $slot,
                mod_id: $mod_id,
                size_delta: $size_delta,
            }
        }};
        ($idx:expr, $size_delta:expr) => {{
            mark!($idx, $idx, $size_delta)
        }};
    }

    macro_rules! marks {
        ($size:expr; $($slot:expr, $mod_id:expr, $size_delta:expr);+) => {{
            let mut marks = VecDeque::<Watermark>::new();
            $(
                marks.push_back(mark!($slot, $mod_id, $size_delta));
            )+
            Watermarks {
                marks,
                size_at_last_capture: $size,
                mark_size: 250,
                max_ledger_size: 1000,
            }
        }};
    }
    macro_rules! truncate_ledger {
        ($slot:expr, $mod_id:expr, $watermarks:ident, $mark:expr, $size:ident) => {{
            // This step is usually performed in _actual_ ledger truncate method
            $size -= $mark.size_delta;
            debug!("Truncated ledger to size {} -> {:#?}", $size, $watermarks);
        }};
    }

    macro_rules! adjust_last_capture {
        ($watermarks:ident, $size:ident, $mark:ident ) => {{
            $watermarks.size_at_last_capture =
                $size - $mark.as_ref().map(|x| x.size_delta).unwrap_or(0);
        }};
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
            let (mark, captured) = watermarks.get_truncation_mark(size, i, i);
            assert!(
                mark.is_none(),
                "Expected no truncation mark at size {}",
                size
            );
            if captured {
                watermarks.size_at_last_capture = size;
            }
        }

        assert_eq!(watermarks, marks!(750; 4, 4, 250; 9, 9, 250; 14, 14, 250));

        // 2. Hit ledger max size
        size += STEP_SIZE;
        let (mark, captured) = watermarks.get_truncation_mark(size, 20, 20);
        assert_eq!(mark, Some(mark!(4, 4, 250)));
        assert!(captured, "Expected to capture a truncation mark");
        adjust_last_capture!(watermarks, size, mark);

        assert_eq!(
            watermarks,
            marks!(750; 9, 9, 250; 14, 14, 250; 20, 20, 250)
        );

        truncate_ledger!(20, 20, watermarks, mark.unwrap(), size);

        // 3. Go up to right below the next truncation mark (also ledger max size)
        for i in 21..=24 {
            size += STEP_SIZE;
            let (mark, captured) = watermarks.get_truncation_mark(size, i, i);
            assert!(
                mark.is_none(),
                "Expected no truncation mark at size {}",
                size
            );
            assert!(!captured, "Expected no truncation mark captured");
        }
        assert_eq!(
            watermarks,
            marks!(750; 9, 9, 250; 14, 14, 250; 20, 20, 250)
        );

        // 4. Hit next truncation mark (also ledger max size)
        size += STEP_SIZE;
        let (mark, captured) = watermarks.get_truncation_mark(size, 25, 25);
        assert_eq!(mark, Some(mark!(9, 9, 250)));
        assert!(captured, "Expected to capture a truncation mark");
        adjust_last_capture!(watermarks, size, mark);

        truncate_ledger!(25, 25, watermarks, mark.unwrap(), size);
        assert_eq!(
            watermarks,
            marks!(750; 14, 14, 250; 20, 20, 250; 25, 25, 250)
        );

        // 5. Go past 3 truncation marks
        for i in 26..=40 {
            size += STEP_SIZE;
            let (mark, captured) = watermarks.get_truncation_mark(size, i, i);
            if captured {
                adjust_last_capture!(watermarks, size, mark);
            }
            if mark.is_some() {
                truncate_ledger!(i, i, watermarks, mark.unwrap(), size);
            }
        }

        assert_eq!(
            watermarks,
            marks!(750; 30, 30, 250; 35, 35, 250; 40, 40, 250)
        );
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
            marks!(900; 50, 50, 300; 100, 100, 300; 150, 150, 300)
        );
    }
}
