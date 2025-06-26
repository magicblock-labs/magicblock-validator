#![allow(unused)]

pub mod config;
pub mod errors;
pub mod traits;
mod truncator;
mod watermarks;

use config::{ExistingLedgerState, LedgerSizeManagerConfig, ResizePercentage};
use errors::{LedgerSizeManagerError, LedgerSizeManagerResult};
use log::*;
use std::{collections::VecDeque, sync::Arc, time::Duration};
use traits::ManagableLedger;
use watermarks::{Watermark, Watermarks};

use magicblock_metrics::metrics;
use solana_sdk::clock::Slot;
use thiserror::Error;
use tokio::{
    task::{JoinError, JoinHandle},
    time::interval,
};
use tokio_util::sync::CancellationToken;

use crate::Ledger;

enum ServiceState {
    Created {
        size_check_interval: Duration,
        resize_percentage: ResizePercentage,
        max_ledger_size: u64,
        existing_ledger_state: Option<ExistingLedgerState>,
    },
    Running {
        cancellation_token: CancellationToken,
        worker_handle: JoinHandle<()>,
    },
    Stopped {
        worker_handle: JoinHandle<()>,
    },
}

pub struct LedgerSizeManager<T: ManagableLedger> {
    ledger: Arc<T>,
    service_state: ServiceState,
}

impl<T: ManagableLedger> LedgerSizeManager<T> {
    pub(crate) fn new(
        ledger: Arc<T>,
        ledger_state: Option<ExistingLedgerState>,
        config: LedgerSizeManagerConfig,
    ) -> Self {
        LedgerSizeManager {
            ledger,
            service_state: ServiceState::Created {
                size_check_interval: Duration::from_millis(
                    config.size_check_interval_ms,
                ),
                resize_percentage: config.resize_percentage,
                max_ledger_size: config.max_size,
                existing_ledger_state: ledger_state,
            },
        }
    }

    pub fn try_start(self) -> LedgerSizeManagerResult<Self> {
        if let ServiceState::Created {
            size_check_interval,
            resize_percentage,
            max_ledger_size,
            mut existing_ledger_state,
        } = self.service_state
        {
            let cancellation_token = CancellationToken::new();
            let worker_handle = {
                let ledger = self.ledger.clone();
                ledger.initialize_lowest_cleanup_slot()?;

                let mut interval = interval(size_check_interval);

                let mut watermarks = None::<Watermarks>;
                let mut cancellation_token = cancellation_token.clone();
                tokio::spawn(async move {
                    loop {
                        tokio::select! {
                            _ = cancellation_token.cancelled() => {
                                return;
                            }
                            _ = interval.tick() => {
                                    let ledger_size = Self::tick(
                                        &ledger,
                                        &mut watermarks,
                                        &resize_percentage,
                                        max_ledger_size,
                                        &mut existing_ledger_state,
                                    ).await;
                                    if let Some(ledger_size) = ledger_size {
                                        metrics::set_ledger_size(ledger_size);
                                    }
                                }
                        }
                    }
                })
            };
            Ok(Self {
                ledger: self.ledger,
                service_state: ServiceState::Running {
                    cancellation_token,
                    worker_handle,
                },
            })
        } else {
            warn!("LedgerSizeManager already running, no need to start.");
            Ok(self)
        }
    }

    async fn tick(
        ledger: &Arc<T>,
        watermarks: &mut Option<Watermarks>,
        resize_percentage: &ResizePercentage,
        max_ledger_size: u64,
        existing_ledger_state: &mut Option<ExistingLedgerState>,
    ) -> Option<u64> {
        // This function is called on each tick to manage the ledger size.
        // It checks the current ledger size and truncates it if necessary.
        let Ok(ledger_size) = ledger.storage_size() else {
            error!("Failed to get ledger size, cannot manage its size");
            return None;
        };

        // If we restarted with an existing ledger we need to make sure that the
        // ledger size is not far above the max size before we can
        // start using the watermark strategy.
        if watermarks.is_none()
            && existing_ledger_state.is_some()
            && ledger_size > max_ledger_size
        {
            warn!(
                "Ledger size {} is above the max size {}, \
                waiting for truncation before using watermarks.",
                ledger_size, max_ledger_size
            );
            Self::ensure_initial_max_ledger_size_below(
                ledger,
                ledger_size,
                max_ledger_size,
            )
            .await;
            return Some(ledger_size);
        }

        // We either started new or trimmed the existing ledger to
        // below the max size and now will keep it so using watermarks.
        let wms = watermarks.get_or_insert_with(|| {
            Watermarks::new(
                resize_percentage,
                max_ledger_size,
                existing_ledger_state.take(),
            )
        });

        if let Some(mark) = wms.get_truncation_mark(
            ledger_size,
            ledger.last_slot(),
            ledger.last_mod_id(),
        ) {
            Self::truncate_ledger(ledger, mark);
            ledger.storage_size().ok()
        } else {
            Some(ledger_size)
        }
    }

    pub fn stop(self) -> Self {
        match self.service_state {
            ServiceState::Running {
                cancellation_token,
                worker_handle,
            } => {
                cancellation_token.cancel();
                Self {
                    ledger: self.ledger,
                    service_state: ServiceState::Stopped { worker_handle },
                }
            }
            _ => {
                warn!("LedgerSizeManager is not running, cannot stop.");
                self
            }
        }
    }

    async fn ensure_initial_max_ledger_size_below(
        ledger: &Arc<T>,
        current_size: u64,
        max_size: u64,
    ) {
        let total_slots = ledger.last_slot();
        let avg_size_per_slot = current_size as f64 / total_slots as f64;
        let max_slots = (max_size as f64 / avg_size_per_slot).floor() as Slot;
        let cut_slots = total_slots - max_slots;
        let current_lowest_slot = ledger.get_lowest_cleanup_slot();

        let max_slot = (current_lowest_slot + cut_slots).min(total_slots);

        ledger.truncate_fat_ledger(max_slot).await;
    }

    async fn truncate_ledger(ledger: &Arc<T>, mark: Watermark) {
        debug!(
            "Truncating ledger at slot {}, size {}",
            mark.slot, mark.size
        );
        ledger.compact_slot_range(mark.slot).await;
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use async_trait::async_trait;

    use super::*;
    use crate::{errors::LedgerResult, Ledger};

    // -----------------
    // ManageableLedgerMock
    // -----------------
    const BYTES_PER_SLOT: u64 = 100;
    struct ManageableLedgerMock {
        first_slot: Mutex<Slot>,
        last_slot: Mutex<Slot>,
        last_mod_id: Mutex<u64>,
    }

    impl ManageableLedgerMock {
        fn new(first_slot: Slot, last_slot: Slot, last_mod_id: u64) -> Self {
            ManageableLedgerMock {
                first_slot: Mutex::new(first_slot),
                last_slot: Mutex::new(last_slot),
                last_mod_id: Mutex::new(last_mod_id),
            }
        }

        fn slots(&self) -> Slot {
            let first_slot = *self.first_slot.lock().unwrap();
            let last_slot = *self.last_slot.lock().unwrap();
            last_slot - first_slot
        }

        fn add_slots(&self, slots: Slot) {
            let mut last_slot = self.last_slot.lock().unwrap();
            *last_slot += slots;
        }
    }

    #[async_trait]
    impl ManagableLedger for ManageableLedgerMock {
        fn storage_size(&self) -> LedgerResult<u64> {
            Ok(self.slots() * BYTES_PER_SLOT)
        }

        fn last_slot(&self) -> Slot {
            *self.last_slot.lock().unwrap()
        }

        fn last_mod_id(&self) -> u64 {
            *self.last_mod_id.lock().unwrap()
        }

        fn initialize_lowest_cleanup_slot(&self) -> LedgerResult<()> {
            Ok(())
        }

        fn get_lowest_cleanup_slot(&self) -> Slot {
            *self.first_slot.lock().unwrap()
        }

        async fn compact_slot_range(&self, to: Slot) {
            assert!(to >= self.last_slot());
            *self.first_slot.lock().unwrap() = to;
        }

        async fn truncate_fat_ledger(&self, lowest_slot: Slot) {
            *self.first_slot.lock().unwrap() = lowest_slot;
        }
    }

    // -----------------
    // Tests
    // -----------------
    #[tokio::test]
    async fn test_ledger_size_manager() {}
}
