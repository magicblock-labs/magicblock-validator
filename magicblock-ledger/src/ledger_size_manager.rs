#![allow(unused)]
use log::*;
use std::{collections::VecDeque, sync::Arc, time::Duration};

use magicblock_metrics::metrics;
use solana_sdk::clock::Slot;
use thiserror::Error;
use tokio::{
    task::{JoinError, JoinHandle},
    time::interval,
};
use tokio_util::sync::CancellationToken;

use crate::Ledger;

// -----------------
// LedgerManagerError
// -----------------
#[derive(Error, Debug)]
pub enum LedgerSizeManagerError {
    #[error(transparent)]
    LedgerError(#[from] crate::errors::LedgerError),
    #[error("Ledger needs to be provided to start the manager")]
    LedgerNotProvided,
    #[error("Failed to join worker: {0}")]
    JoinError(#[from] JoinError),
}
pub type LedgerSizeManagerResult<T> = Result<T, LedgerSizeManagerError>;

// -----------------
// LedgerSizeManagerConfig
// -----------------
/// Percentage of ledger to keep when resizing.
pub enum ResizePercentage {
    /// Keep 75% of the ledger size.
    Large,
    /// Keep 66% of the ledger size.
    Medium,
    /// Keep 50% of the ledger size.
    Small,
}

impl ResizePercentage {
    /// The portion of ledger we cut on each resize.
    pub fn watermark_size_percent(&self) -> u64 {
        use ResizePercentage::*;
        match self {
            Large => 25,
            Medium => 34,
            Small => 50,
        }
    }

    /// The number of watermarks to track
    pub fn watermark_count(&self) -> u64 {
        use ResizePercentage::*;
        match self {
            Large => 3,
            Medium => 2,
            Small => 1,
        }
    }
}

pub struct LedgerSizeManagerConfig {
    /// Max ledger size to maintain.
    /// The [LedgerSizeManager] will attempt to respect this size,, but
    /// it may grow larger temporarily in between size checks.
    pub max_size: u64,

    /// Interval at which the size is checked in milliseconds.
    pub size_check_interval_ms: u64,

    /// Percentage of the ledger to keep when resizing
    pub resize_percentage: ResizePercentage,
}

// -----------------
// Watermarks
// -----------------
#[derive(Debug, PartialEq, Eq)]
struct Watermark {
    /// The slot at which this watermark was captured
    slot: u64,
    /// Account mod ID at which this watermark was captured
    mod_id: u64,
    /// The size of the ledger when this watermark was captured
    size: u64,
}

#[derive(Debug, PartialEq, Eq)]
struct Watermarks {
    /// The watermarks captured
    marks: VecDeque<Watermark>,
    /// The maximum number of watermarks to keep
    count: u64,
    /// The targeted size difference for each watermark
    mark_size: u64,
    /// The maximum ledger size to maintain
    max_ledger_size: u64,
}

pub struct ExistingLedgerState {
    /// The current size of the ledger
    size: u64,
    /// The last slot in the ledger at time of restart
    slot: Slot,
    /// The last account mod ID in the ledger at time of restart
    mod_id: u64,
}

impl Watermarks {
    /// Creates a new set of watermarks based on the resize percentage and max ledger size.
    /// - * `percentage`: The resize percentage to use.
    /// - * `max_ledger_size`: The maximum size of the ledger to try to maintain.
    /// - * `ledger_state`: The current ledger state which is
    ///      only available during restart with an existing ledger
    fn new(
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

// -----------------
// LedgerSizeManager
// -----------------
enum ServiceState {
    Created {
        size_check_interval: Duration,
        resize_percentage: ResizePercentage,
    },
    Running {
        cancellation_token: CancellationToken,
        worker_handle: JoinHandle<()>,
    },
    Stopped {
        worker_handle: JoinHandle<()>,
    },
}

pub struct LedgerSizeManager {
    ledger: Option<Arc<Ledger>>,
    watermarks: Watermarks,
    service_state: ServiceState,
}

impl LedgerSizeManager {
    pub(crate) fn new(
        ledger: Option<Arc<Ledger>>,
        ledger_state: Option<ExistingLedgerState>,
        config: LedgerSizeManagerConfig,
    ) -> Self {
        let watermarks = Watermarks::new(
            &config.resize_percentage,
            config.max_size,
            ledger_state,
        );
        LedgerSizeManager {
            ledger,
            watermarks,
            service_state: ServiceState::Created {
                size_check_interval: Duration::from_millis(
                    config.size_check_interval_ms,
                ),
                resize_percentage: config.resize_percentage,
            },
        }
    }

    fn ensure_initial_max_ledger_size(&self) {
        // TODO: @@@ wait for fix/ledger/delete-using-compaction-filter to be merged which
        // includes a _fat_ ledger truncate
        // We will run that first to get below the ledger max size _before_ switching to
        // the watermark strategy.
    }

    pub fn try_start(self) -> LedgerSizeManagerResult<Self> {
        let Some(ledger) = self.ledger.take() else {
            return Err(LedgerSizeManagerError::LedgerNotProvided);
        };
        if let ServiceState::Created {
            size_check_interval,
            resize_percentage,
        } = self.service_state
        {
            let cancellation_token = CancellationToken::new();
            let worker_handle = {
                ledger.initialize_lowest_cleanup_slot()?;
                let mut interval = interval(size_check_interval);
                let cancellation_token = cancellation_token.clone();
                tokio::spawn(async move {
                    loop {
                        tokio::select! {
                            _ = cancellation_token.cancelled() => {
                                return;
                            }
                            _ = interval.tick() => {
                                    let Ok(ledger_size) = ledger.storage_size() else {
                                                eprintln!(
                                            "Failed to get ledger size, cannot start LedgerSizeManager"
                                        );
                                        continue;
                                    };
                                    metrics::set_ledger_size(ledger_size);
                                    if ledger_size > self.watermarks.max_ledger_size {
                                        self.ensure_initial_max_ledger_size();
                                        continue;
                                    }
                                    if self.watermarks.is_empty() {
                                    self.watermarks = Watermarks::new(
                                        &resize_percentage,
                                        self.watermarks.max_ledger_size,
                                        Some(ExistingLedgerState {
                                            size: ledger_size,
                                            slot: ledger.last_slot(),
                                            mod_id: ledger.last_mod_id(),
                                        }),
                                    );
                                    }



                                    if let Some(mark) = self.get_truncation_mark(
                                        ledger_size,
                                        ledger.last_slot(),
                                        ledger.last_mod_id(),
                                    ) {
                                        self.truncate(&ledger, mark);
                                    }
                                }
                        }
                    }
                })
            };
            self.service_state = ServiceState::Running {
                cancellation_token,
                worker_handle,
            };
            todo!()
        } else {
            warn!("LedgerSizeManager already running, no need to start.");
            todo!()
        }
    }

    fn get_truncation_mark(
        &mut self,
        ledger_size: u64,
        slot: Slot,
        mod_id: u64,
    ) -> Option<Watermark> {
        if ledger_size == 0 {
            return None;
        }
        if self.watermarks.reached_max(ledger_size) {
            debug!(
                "Ledger size {} reached maximum size {}, resizing...",
                ledger_size, self.watermarks.max_ledger_size
            );
            self.watermarks.consume_next()
        } else {
            self.watermarks.update(slot, mod_id, ledger_size);
            None
        }
    }

    fn truncate(&self, ledger: &Arc<Ledger>, mark: Watermark) {
        // This is where the truncation logic would go.
        // For now, we just print the truncation mark.
        debug!(
            "Truncating ledger at slot {}, mod_id {}, size {}",
            mark.slot, mark.mod_id, mark.size
        );
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
        ($slot:expr, $mod_id:expr, $sut:ident, $mark:expr, $size:ident) => {{
            // These steps are usually performed in _actual_ ledger truncate method
            $size -= $mark.size;
            $sut.watermarks.adjust_for_truncation();
            $sut.watermarks.update($slot, $mod_id, $size);
            debug!(
                "Truncated ledger to size {} -> {:#?}",
                $size, $sut.watermarks
            );
        }}
    }

    #[test]
    fn test_size_manager_new_ledger() {
        init_logger!();

        let percentage = ResizePercentage::Large;
        const MAX_SIZE: u64 = 1_000;
        const STEP_SIZE: u64 = MAX_SIZE / 20;
        let mut sut = LedgerSizeManager::new(
            None,
            None,
            LedgerSizeManagerConfig {
                max_size: MAX_SIZE,
                size_check_interval_ms: 1000,
                resize_percentage: percentage,
            },
        );

        // 1. Go up to right below the ledger size
        let mut size = 0;
        for i in 0..19 {
            size += STEP_SIZE;
            let mark = sut.get_truncation_mark(size, i, i);
            assert!(
                mark.is_none(),
                "Expected no truncation mark at size {}",
                size
            );
        }

        assert_eq!(sut.watermarks, marks!(4, 4, 250; 9, 9, 500; 14, 14, 750));

        // 2. Hit ledger max size
        size += STEP_SIZE;
        let mark = sut.get_truncation_mark(size, 20, 20);
        assert_eq!(sut.watermarks, marks!(9, 9, 500; 14, 14, 750));
        assert_eq!(mark, Some(mark!(4, 4, 250)));

        truncate_ledger!(20, 20, sut, mark.unwrap(), size);
        assert_eq!(sut.watermarks, marks!(9, 9, 250; 14, 14, 500; 20, 20, 750));

        // 3. Go up to right below the next truncation mark (also ledger max size)
        for i in 21..=24 {
            size += STEP_SIZE;
            let mark = sut.get_truncation_mark(size, i, i);
            assert!(
                mark.is_none(),
                "Expected no truncation mark at size {}",
                size
            );
        }
        assert_eq!(sut.watermarks, marks!(9, 9, 250; 14, 14, 500; 20, 20, 750));

        // 4. Hit next truncation mark (also ledger max size)
        size += STEP_SIZE;
        let mark = sut.get_truncation_mark(size, 25, 25);
        assert_eq!(mark, Some(mark!(9, 9, 250)));

        truncate_ledger!(25, 25, sut, mark.unwrap(), size);
        assert_eq!(
            sut.watermarks,
            marks!(14, 14, 250; 20, 20, 500; 25, 25, 750)
        );

        // 5. Go past 3 truncation marks
        for i in 26..=40 {
            size += STEP_SIZE;
            let mark = sut.get_truncation_mark(size, i, i);
            if mark.is_some() {
                truncate_ledger!(i, i, sut, mark.unwrap(), size);
            }
        }

        assert_eq!(
            sut.watermarks,
            marks!(30, 30, 250; 35, 35, 500; 40, 40, 750)
        );
    }

    #[test]
    fn test_size_manager_existing_ledger() {
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
        let mut sut = LedgerSizeManager::new(
            None,
            Some(ledger_state),
            LedgerSizeManagerConfig {
                max_size: MAX_SIZE,
                size_check_interval_ms: 1000,
                resize_percentage: percentage,
            },
        );

        // Initial watermarks should be based on the existing ledger state
        assert_eq!(
            sut.watermarks,
            marks!(50, 50, 300; 100, 100, 600; 150, 150, 900)
        );
    }
}
