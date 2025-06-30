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
        if watermarks.is_none() && existing_ledger_state.is_some() {
            warn!(
                "Ledger size {} is above the max size {}, \
                waiting for truncation before using watermarks.",
                ledger_size, max_ledger_size
            );

            if ledger_size > max_ledger_size {
                Self::ensure_initial_max_ledger_size_below(
                    ledger,
                    ledger_size,
                    max_ledger_size,
                )
                .await;
            }
            watermarks.replace(Watermarks::new(
                resize_percentage,
                max_ledger_size,
                existing_ledger_state.take(),
            ));
            return Some(ledger_size);
        }

        // If we started with an existing ledger state we already added watermarks,
        // otherwise we do this here the during the first loop iteration
        let wms = watermarks.get_or_insert_with(|| {
            Watermarks::new(resize_percentage, max_ledger_size, None)
        });

        let mut ledger_size = ledger_size;
        // If ledger exceeded size we downsize until we either reach below the max size
        // or run out of watermarks.
        loop {
            let last_slot = ledger.last_slot();

            if let Some(mark) = wms.get_truncation_mark(
                ledger_size,
                last_slot,
                ledger.last_mod_id(),
            ) {
                Self::truncate_ledger(ledger, mark).await;

                if let Ok(ls) = ledger.storage_size() {
                    ledger_size = ls;
                } else {
                    break Some(ledger_size);
                }
            } else {
                break Some(ledger_size);
            }
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
            "Truncating ledger up to slot {} gaining {} bytes",
            mark.slot, mark.size_delta
        );
        ledger.compact_slot_range(mark.slot).await;
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use async_trait::async_trait;
    use test_tools_core::init_logger;

    use super::*;
    use crate::{errors::LedgerResult, Ledger};

    // -----------------
    // ManageableLedgerMock
    // -----------------
    const BYTES_PER_SLOT: u64 = 100;
    struct ManageableLedgerMock {
        lowest_slot: Mutex<Slot>,
        last_slot: Mutex<Slot>,
        last_mod_id: Mutex<u64>,
    }

    impl ManageableLedgerMock {
        fn new(first_slot: Slot, last_slot: Slot, last_mod_id: u64) -> Self {
            ManageableLedgerMock {
                lowest_slot: Mutex::new(first_slot),
                last_slot: Mutex::new(last_slot),
                last_mod_id: Mutex::new(last_mod_id),
            }
        }

        fn slots(&self) -> Slot {
            let first_slot = *self.lowest_slot.lock().unwrap();
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
            *self.lowest_slot.lock().unwrap()
        }

        async fn compact_slot_range(&self, to: Slot) {
            let lowest_slot = self.get_lowest_cleanup_slot();
            assert!(
                to >= lowest_slot,
                "{to} must be >= last slot {lowest_slot}",
            );
            debug!("Setting lowest cleanup slot to {}", to);
            *self.lowest_slot.lock().unwrap() = to;
        }

        async fn truncate_fat_ledger(&self, lowest_slot: Slot) {
            *self.lowest_slot.lock().unwrap() = lowest_slot;
        }
    }

    // -----------------
    // Tests
    // -----------------
    #[tokio::test]
    async fn test_ledger_size_manager_new_ledger() {
        init_logger!();

        let ledger = Arc::new(ManageableLedgerMock::new(0, 0, 0));
        let mut watermarks = None::<Watermarks>;
        let resize_percentage = ResizePercentage::Large;
        let max_ledger_size = 800;
        let mut existing_ledger_state = None::<ExistingLedgerState>;

        macro_rules! tick {
            ($tick:expr) => {{
                let ledger_size =
                    LedgerSizeManager::<ManageableLedgerMock>::tick(
                        &ledger,
                        &mut watermarks,
                        &resize_percentage,
                        max_ledger_size,
                        &mut existing_ledger_state,
                    )
                    .await
                    .unwrap();
                debug!(
                    "Ledger after tick {}: Size {} {:#?}",
                    $tick, ledger_size, watermarks
                );
                ledger_size
            }};
        }
        info!("Slot: 0, New Ledger");
        let ledger_size = tick!(1);
        assert_eq!(ledger_size, 0);

        info!("Slot: 1 added 1 slot -> 100 bytes");
        ledger.add_slots(1);
        let ledger_size = tick!(2);
        assert_eq!(ledger_size, 100);

        info!("Slot: 4, added 3 slots -> 400 bytes marked (delta: 400)");
        ledger.add_slots(3);
        let ledger_size = tick!(3);
        assert_eq!(ledger_size, 400);

        info!("Slot: 6, added 2 slots -> 600 bytes marked (delta: 200)");
        ledger.add_slots(2);
        let ledger_size = tick!(4);
        assert_eq!(ledger_size, 600);

        info!("Slot: 7, added 1 slot -> 700 bytes");
        ledger.add_slots(1);
        let ledger_size = tick!(5);
        assert_eq!(ledger_size, 700);

        // Here we go to 900 and truncate using the first watermark which removes 400 bytes
        info!("Slot 9, added 2 slots -> 900 bytes marked (delta: 300) -> remove 400 -> 500 bytes ");
        ledger.add_slots(2);
        let ledger_size = tick!(6);
        assert_eq!(ledger_size, 500);

        info!("Slot 10, added 1 slot -> 600 bytes");
        ledger.add_slots(1);
        let ledger_size = tick!(7);
        assert_eq!(ledger_size, 600);

        info!("Slot 14, added 4 slots -> 1000 bytes marked (delta: 500) -> remove 200 -> remove 300");
        ledger.add_slots(4);
        let ledger_size = tick!(8);
        assert_eq!(ledger_size, 500);
    }

    #[tokio::test]
    async fn test_ledger_size_manager_existing_ledger_below_max_size() {
        init_logger!();

        let ledger = Arc::new(ManageableLedgerMock::new(0, 6, 6));
        let mut watermarks = None::<Watermarks>;
        let resize_percentage = ResizePercentage::Large;
        let max_ledger_size = 1000;
        let mut existing_ledger_state = Some(ExistingLedgerState {
            size: 600,
            slot: 6,
            mod_id: 6,
        });

        macro_rules! tick {
            ($tick:expr) => {{
                let ledger_size =
                    LedgerSizeManager::<ManageableLedgerMock>::tick(
                        &ledger,
                        &mut watermarks,
                        &resize_percentage,
                        max_ledger_size,
                        &mut existing_ledger_state,
                    )
                    .await
                    .unwrap();
                debug!(
                    "Ledger after tick {}: Size {} {:#?}",
                    $tick, ledger_size, watermarks
                );
                ledger_size
            }};
        }

        info!("Slot: 6, existing ledger");
        let ledger_size = tick!(1);
        assert_eq!(ledger_size, 600);
        assert_eq!(
            watermarks.as_ref().unwrap(),
            &Watermarks {
                marks: [
                    Watermark {
                        slot: 2,
                        mod_id: 2,
                        size_delta: 200,
                    },
                    Watermark {
                        slot: 4,
                        mod_id: 4,
                        size_delta: 200,
                    },
                    Watermark {
                        slot: 6,
                        mod_id: 6,
                        size_delta: 200,
                    },
                ]
                .into(),
                size_at_last_capture: 600,
                count: 3,
                mark_size: 250,
                max_ledger_size: 1000,
            },
        );

        info!("Slot: 10, added 1 slot -> 700 bytes");
        ledger.add_slots(1);
        let ledger_size = tick!(2);
        assert_eq!(ledger_size, 700);

        info!("Slot: 12, added 2 slots -> 900 bytes marked (delta: 200)");
        ledger.add_slots(2);
        let ledger_size = tick!(3);
        assert_eq!(ledger_size, 900);

        info!("Slot: 14, added 2 slots -> 1100 bytes marked (delta: 200) -> remove 200 -> 900 bytes");
        ledger.add_slots(2);
        let ledger_size = tick!(4);
        assert_eq!(ledger_size, 900);
    }
}
