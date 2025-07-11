#![allow(unused)]

pub mod config;
pub mod errors;
pub mod traits;
mod truncator;
mod watermarks;

use std::{collections::VecDeque, sync::Arc, time::Duration};

use config::{ExistingLedgerState, LedgerSizeManagerConfig, ResizePercentage};
use errors::{LedgerSizeManagerError, LedgerSizeManagerResult};
use log::*;
use magicblock_bank::bank::Bank;
use magicblock_core::traits::FinalityProvider;
use magicblock_metrics::metrics;
use solana_sdk::clock::Slot;
use thiserror::Error;
use tokio::{
    task::{JoinError, JoinHandle},
    time::interval,
};
use tokio_util::sync::CancellationToken;
use traits::ManagableLedger;
use truncator::Truncator;
use watermarks::{Watermark, Watermarks};

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

pub type TruncatingLedgerSizeManager = LedgerSizeManager<Truncator, Bank>;
pub struct LedgerSizeManager<T: ManagableLedger, U: FinalityProvider> {
    ledger: Arc<T>,
    finality_provider: Arc<U>,
    service_state: Option<ServiceState>,
}

impl<T: ManagableLedger, U: FinalityProvider> LedgerSizeManager<T, U> {
    pub fn new_from_ledger(
        ledger: Arc<Ledger>,
        finality_provider: Arc<Bank>,
        ledger_state: Option<ExistingLedgerState>,
        config: LedgerSizeManagerConfig,
    ) -> LedgerSizeManager<Truncator, Bank> {
        let managed_ledger = Truncator { ledger };
        LedgerSizeManager::new(
            Arc::new(managed_ledger),
            finality_provider,
            ledger_state,
            config,
        )
    }

    pub(crate) fn new(
        managed_ledger: Arc<T>,
        finality_provider: Arc<U>,
        ledger_state: Option<ExistingLedgerState>,
        config: LedgerSizeManagerConfig,
    ) -> Self {
        LedgerSizeManager {
            ledger: managed_ledger,
            finality_provider,
            service_state: Some(ServiceState::Created {
                size_check_interval: Duration::from_millis(
                    config.size_check_interval_ms,
                ),
                resize_percentage: config.resize_percentage,
                max_ledger_size: config.max_size,
                existing_ledger_state: ledger_state,
            }),
        }
    }

    pub fn try_start(&mut self) -> LedgerSizeManagerResult<()> {
        if let Some(ServiceState::Created {
            size_check_interval,
            resize_percentage,
            max_ledger_size,
            mut existing_ledger_state,
        }) = self.service_state.take()
        {
            let cancellation_token = CancellationToken::new();
            let worker_handle = {
                let ledger = self.ledger.clone();
                ledger.initialize_lowest_cleanup_slot()?;

                let finality_provider = self.finality_provider.clone();

                let mut cancellation_token = cancellation_token.clone();
                tokio::spawn(async move {
                    let mut interval = interval(size_check_interval);
                    let mut watermarks = None::<Watermarks>;
                    loop {
                        tokio::select! {
                            _ = cancellation_token.cancelled() => {
                                return;
                            }
                            _ = interval.tick() => {
                                    let ledger_size = Self::tick(
                                        &ledger,
                                        &finality_provider,
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
            self.service_state = Some(ServiceState::Running {
                cancellation_token,
                worker_handle,
            });
            Ok(())
        } else {
            warn!("LedgerSizeManager already running, no need to start.");
            Ok(())
        }
    }

    async fn prepare_watermarks_for_existing_ledger(
        ledger: &Arc<T>,
        finality_provider: &Arc<U>,
        existing_ledger_state: ExistingLedgerState,
        resize_percentage: &ResizePercentage,
        max_ledger_size: u64,
    ) -> (Watermarks, u64) {
        let prev_size = existing_ledger_state.size;

        let (adjusted_ledger_size, lowest_slot) =
            if prev_size > max_ledger_size {
                warn!(
                "Existing ledger size {} is above the max size {}, \
                waiting for truncation before using watermarks.",
                prev_size, max_ledger_size
            );

                Self::ensure_initial_max_ledger_size_below(
                    ledger,
                    finality_provider,
                    &existing_ledger_state,
                    resize_percentage,
                    max_ledger_size,
                )
                .await
            } else {
                (prev_size, ledger.get_lowest_cleanup_slot())
            };

        let mut marks = Watermarks::new(
            resize_percentage,
            max_ledger_size,
            Some(existing_ledger_state),
        );
        marks.size_at_last_capture = adjusted_ledger_size;
        // Remove watermarks that are below the lowest cleanup slot
        marks.marks.retain(|mark| mark.slot > lowest_slot);
        
        (marks, adjusted_ledger_size)
    }

    async fn tick(
        ledger: &Arc<T>,
        finality_provider: &Arc<U>,
        watermarks: &mut Option<Watermarks>,
        resize_percentage: &ResizePercentage,
        max_ledger_size: u64,
        existing_ledger_state: &mut Option<ExistingLedgerState>,
    ) -> Option<u64> {
        // If we restarted with an existing ledger we need to make sure that the
        // ledger size is not far above the max size before we can
        // start using the watermark strategy.
        // NOTE: that watermarks are set during the first tick
        if watermarks.is_none() {
            if let Some(existing_ledger_state) = existing_ledger_state.take() {
                let (prepared_watermarks, adjusted_ledger_size) = Self::prepare_watermarks_for_existing_ledger(
                    ledger,
                    finality_provider,
                    existing_ledger_state,
                    resize_percentage,
                    max_ledger_size,
                ).await;

                watermarks.replace(prepared_watermarks);
                return Some(adjusted_ledger_size);
            }
        }

        // This function is called on each tick to manage the ledger size.
        // It checks the current ledger size and truncates it if necessary.
        let Ok(ledger_size) = ledger.storage_size() else {
            error!("Failed to get ledger size, cannot manage its size");
            return None;
        };

        // If we started with an existing ledger state we already added watermarks
        // above, otherwise we do this here the during the first tick
        let mut wms = watermarks.get_or_insert_with(|| {
            Watermarks::new(resize_percentage, max_ledger_size, None)
        });

        let mut ledger_size = ledger_size;
        // If ledger exceeded size we downsize until we either reach below the max size
        // or run out of watermarks.
        loop {
            let last_slot = ledger.last_slot();
            let (mark, captured) = wms.get_truncation_mark(
                ledger_size,
                last_slot,
                ledger.last_mod_id(),
            );
            if let Some(mark) = mark {
                let latest_final_slot =
                    finality_provider.get_latest_final_slot();

                let lowest_cleanup_slot = ledger.get_lowest_cleanup_slot();
                if lowest_cleanup_slot >= latest_final_slot {
                    warn!(
                        "Lowest cleanup slot {} is at or above the latest final slot {}. \
                        Cannot truncate above the lowest cleanup slot.",
                        lowest_cleanup_slot, latest_final_slot
                    );
                    wms.push_front(mark);
                    if captured {
                        wms.size_at_last_capture = ledger_size;
                    }
                    return Some(ledger_size);
                }

                if mark.slot > latest_final_slot {
                    warn!("Truncation would remove data at or above the latest final slot {}. \
                           Adjusting truncation for mark: {mark:?} to cut up to the latest final slot.",
                        latest_final_slot);

                    // Estimate the size delta based on the ratio of the slots
                    // that we can remove
                    let original_diff =
                        mark.slot.saturating_sub(lowest_cleanup_slot);
                    let applied_diff =
                        latest_final_slot.saturating_sub(lowest_cleanup_slot);
                    let size_delta = (applied_diff as f64
                        / original_diff as f64
                        * mark.size_delta as f64)
                        as u64;
                    Self::truncate_ledger(
                        ledger,
                        latest_final_slot,
                        size_delta,
                    )
                    .await;
                    wms.size_at_last_capture =
                        wms.size_at_last_capture.saturating_sub(size_delta);

                    // Since we didn't truncate the full mark, we need to put one
                    // back so it will be processed to remove the remaining space
                    // when possible
                    // Otherwise we would process the following mark which would
                    // cause us to truncate too many slots
                    wms.push_front(Watermark {
                        slot: mark.slot,
                        mod_id: mark.mod_id,
                        size_delta: mark.size_delta.saturating_sub(size_delta),
                    });
                } else {
                    Self::truncate_ledger(ledger, mark.slot, mark.size_delta)
                        .await;
                    wms.size_at_last_capture = wms
                        .size_at_last_capture
                        .saturating_sub(mark.size_delta);
                }

                if let Ok(ls) = ledger.storage_size() {
                    ledger_size = ls;
                } else {
                    // If we cannot get the ledger size we guess it
                    ledger_size = ledger_size.saturating_sub(mark.size_delta);
                }
                if captured {
                    wms.size_at_last_capture = ledger_size;
                }
            } else {
                if captured {
                    wms.size_at_last_capture = ledger_size;
                }
                return Some(ledger_size);
            }
        }
    }

    pub fn stop(&mut self) {
        match self.service_state.take() {
            Some(ServiceState::Running {
                cancellation_token,
                worker_handle,
            }) => {
                cancellation_token.cancel();
                self.service_state =
                    Some(ServiceState::Stopped { worker_handle });
            }
            _ => {
                warn!("LedgerSizeManager is not running, cannot stop.");
            }
        }
    }

    /// Downsizes the ledger to the percentage we want to truncate to whenever we reach or
    /// exceed the maximum ledger size.
    /// Returns the adjusted ledger size after truncation and the lowest cleanup slot.
    async fn ensure_initial_max_ledger_size_below(
        ledger: &Arc<T>,
        finality_provider: &Arc<U>,
        existing_ledger_state: &ExistingLedgerState,
        resize_percentage: &ResizePercentage,
        max_size: u64,
    ) -> (u64, Slot) {
        let ExistingLedgerState {
            size: current_size,
            slot: total_slots,
            mod_id,
        } = existing_ledger_state;
        let mut ledger_size = *current_size;
        let mut total_slots = *total_slots;

        let finality_slot = finality_provider.get_latest_final_slot();

        while ledger_size >= max_size {
            let avg_size_per_slot = *current_size as f64 / total_slots as f64;
            let target_size = resize_percentage.upper_mark_size(max_size);
            let target_slot =
                (target_size as f64 / avg_size_per_slot).floor() as Slot;

            let cut_slots = total_slots.saturating_sub(target_slot);
            let current_lowest_slot = ledger.get_lowest_cleanup_slot();

            let max_slot = current_lowest_slot
                .saturating_add(cut_slots)
                .min(total_slots);
            // We can either truncate up to the calculated slot and repeat this until
            // we reach the target size or we can truncate up to the latest final slot
            // and then have to stop.
            if max_slot > finality_slot {
                let lowest_cleanup_slot = ledger.get_lowest_cleanup_slot();
                if lowest_cleanup_slot >= finality_slot {
                    warn!(
                        "Lowest cleanup slot {} is above the latest final slot {}. \
                        Initial truncation cannot truncate above the lowest cleanup slot.",
                        lowest_cleanup_slot, finality_slot
                    );
                    return (ledger_size, lowest_cleanup_slot);
                }

                warn!(
                    "Initial truncation would remove data above the latest final slot {}. \
                    Truncating only up to the latest final slot {}.",
                    finality_slot, max_slot
                );
                ledger.truncate_fat_ledger(finality_slot).await;

                let ledger_size = ledger.storage_size().unwrap_or({
                    (avg_size_per_slot * finality_slot as f64) as u64
                });
                return (ledger_size, ledger.get_lowest_cleanup_slot());
            } else {
                ledger.truncate_fat_ledger(max_slot).await;

                ledger_size = ledger.storage_size().unwrap_or(target_size);
                total_slots -= cut_slots;
            }
        }
        (ledger_size, ledger.get_lowest_cleanup_slot())
    }

    async fn truncate_ledger(ledger: &Arc<T>, slot: Slot, size_delta: u64) {
        debug!(
            "Truncating ledger up to slot {} gaining {} bytes",
            slot, size_delta
        );
        ledger.compact_slot_range(slot).await;
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

    struct FinalityProviderMock {
        finality_slot: Mutex<u64>,
    }

    impl Default for FinalityProviderMock {
        fn default() -> Self {
            FinalityProviderMock {
                finality_slot: Mutex::new(u64::MAX),
            }
        }
    }

    impl FinalityProvider for FinalityProviderMock {
        fn get_latest_final_slot(&self) -> u64 {
            *self.finality_slot.lock().unwrap()
        }
    }

    // -----------------
    // Tests
    // -----------------
    #[tokio::test]
    async fn test_ledger_size_manager_new_ledger() {
        init_logger!();

        let ledger = Arc::new(ManageableLedgerMock::new(0, 0, 0));
        let finality_provider = Arc::new(FinalityProviderMock::default());
        let mut watermarks = None::<Watermarks>;
        let resize_percentage = ResizePercentage::Large;
        let max_ledger_size = 800;
        let mut existing_ledger_state = None::<ExistingLedgerState>;

        macro_rules! tick {
            ($tick:expr) => {{
                let ledger_size = LedgerSizeManager::<
                    ManageableLedgerMock,
                    FinalityProviderMock,
                >::tick(
                    &ledger,
                    &finality_provider,
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
    async fn test_ledger_size_manager_new_ledger_reaching_finality_slot() {
        init_logger!();

        let ledger = Arc::new(ManageableLedgerMock::new(0, 0, 0));
        let finality_provider = Arc::new(FinalityProviderMock {
            finality_slot: Mutex::new(4),
        });
        let mut watermarks = None::<Watermarks>;
        let resize_percentage = ResizePercentage::Large;
        let max_ledger_size = 800;
        let mut existing_ledger_state = None::<ExistingLedgerState>;

        macro_rules! tick {
            () => {{
                let ledger_size = LedgerSizeManager::<
                    ManageableLedgerMock,
                    FinalityProviderMock,
                >::tick(
                    &ledger,
                    &finality_provider,
                    &mut watermarks,
                    &resize_percentage,
                    max_ledger_size,
                    &mut existing_ledger_state,
                )
                .await
                .unwrap();
                debug!("Ledger Size {} {:#?}", ledger_size, watermarks);
                ledger_size
            }};
        }

        macro_rules! wms {
            ($size:expr, $len:expr) => {
                let wms = watermarks.as_ref().unwrap();
                assert_eq!(wms.size_at_last_capture, $size);
                assert_eq!(wms.marks.len(), $len);
            };
        }

        info!("Slot: 0, New Ledger");
        let ledger_size = tick!();
        assert_eq!(ledger_size, 0);

        info!("Slot: 1 added 1 slot -> 100 bytes");
        ledger.add_slots(1);
        let ledger_size = tick!();
        assert_eq!(ledger_size, 100);

        info!("Slot: 2, added 1 slot -> 200 bytes marked (delta: 200)");
        ledger.add_slots(1);
        let ledger_size = tick!();
        assert_eq!(ledger_size, 200);

        info!("Slot: 8, added 6 slots -> 800 bytes marked (delta: 600)");
        ledger.add_slots(6);
        let ledger_size = tick!();
        assert_eq!(ledger_size, 600);

        // It would normally remove 600 bytes, but the finality slot is 4 so
        // it can only remove up to 4 instead 8
        info!("Slot: 12, added 4 slots -> 1000 bytes marked (delta: 400) -> remove 200 -> 800 bytes");
        ledger.add_slots(4);
        let ledger_size = tick!();
        assert_eq!(ledger_size, 800);
        wms!(800, 2);

        info!("Slot: 13, added 1 slot -> 900 bytes -> cannot remove anything");
        ledger.add_slots(1);
        let ledger_size = tick!();
        assert_eq!(ledger_size, 900);
        wms!(800, 2);

        info!("Slot: 14 - 15, adding slots, but finality slot blocks removal until it is increased");
        ledger.add_slots(1);
        let ledger_size = tick!();
        assert_eq!(ledger_size, 1_000);
        wms!(1_000, 3);

        ledger.add_slots(1);
        let ledger_size = tick!();
        assert_eq!(ledger_size, 1_100);
        wms!(1_000, 3);

        *finality_provider.finality_slot.lock().unwrap() = 14;
        let ledger_size = tick!();
        assert_eq!(ledger_size, 700);
        // We cut 400 bytes, so the size at last capture is adjusted down
        wms!(600, 2);

        info!(
            "Slot: 16, added 1 slot -> marked (delta: 200) -> remove 400 -> 400 bytes"
        );
        ledger.add_slots(1);
        let ledger_size = tick!();
        assert_eq!(ledger_size, 400);
        wms!(400, 2);

        info!("Slot: 17-20, added 3 slots -> 700 bytes marked (delta: 300) + set finality 19");
        ledger.add_slots(3);
        *finality_provider.finality_slot.lock().unwrap() = 19;
        let ledger_size = tick!();
        assert_eq!(ledger_size, 700);
        wms!(700, 3);

        info!("Slot: 21, added 1 slot -> remove 200 -> 600 bytes");
        ledger.add_slots(1);
        let ledger_size = tick!();
        assert_eq!(ledger_size, 600);
    }

    #[tokio::test]
    async fn test_ledger_size_manager_existing_ledger_below_max_size() {
        init_logger!();

        let ledger = Arc::new(ManageableLedgerMock::new(0, 6, 6));
        let finality_provider = Arc::new(FinalityProviderMock::default());
        let mut watermarks = None::<Watermarks>;
        let resize_percentage = ResizePercentage::Large;
        let max_ledger_size = 1000;
        let mut existing_ledger_state = Some(ExistingLedgerState {
            size: 600,
            slot: 6,
            mod_id: 6,
        });

        macro_rules! tick {
            () => {{
                let ledger_size = LedgerSizeManager::<
                    ManageableLedgerMock,
                    FinalityProviderMock,
                >::tick(
                    &ledger,
                    &finality_provider,
                    &mut watermarks,
                    &resize_percentage,
                    max_ledger_size,
                    &mut existing_ledger_state,
                )
                .await
                .unwrap();
                debug!("Ledger Size {} {:#?}", ledger_size, watermarks);
                ledger_size
            }};
        }

        info!("Slot: 6, existing ledger");
        let ledger_size = tick!();
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
                mark_size: 250,
                max_ledger_size: 1000,
            },
        );

        info!("Slot: 7, added 1 slot -> 700 bytes");
        ledger.add_slots(1);
        let ledger_size = tick!();
        assert_eq!(ledger_size, 700);

        info!("Slot: 9, added 2 slots -> 900 bytes marked (delta: 200)");
        ledger.add_slots(2);
        let ledger_size = tick!();
        assert_eq!(ledger_size, 900);

        info!("Slot: 12, added 3 slots -> 1200 bytes marked (delta: 300) -> remove 200 -> 1000 bytes -> remove 200 -> 800 bytes");
        ledger.add_slots(3);
        let ledger_size = tick!();
        assert_eq!(ledger_size, 800);
    }

    #[tokio::test]
    async fn test_ledger_size_manager_existing_ledger_above_max_size() {
        init_logger!();

        let ledger = Arc::new(ManageableLedgerMock::new(0, 12, 12));
        let finality_provider = Arc::new(FinalityProviderMock::default());
        let mut watermarks = None::<Watermarks>;
        let resize_percentage = ResizePercentage::Large;
        let max_ledger_size = 1000;
        let mut existing_ledger_state = Some(ExistingLedgerState {
            size: 1200,
            slot: 12,
            mod_id: 12,
        });

        macro_rules! tick {
            () => {{
                let ledger_size = LedgerSizeManager::<
                    ManageableLedgerMock,
                    FinalityProviderMock,
                >::tick(
                    &ledger,
                    &finality_provider,
                    &mut watermarks,
                    &resize_percentage,
                    max_ledger_size,
                    &mut existing_ledger_state,
                )
                .await
                .unwrap();
                debug!("Ledger Size {} {:#?}", ledger_size, watermarks);
                ledger_size
            }};
        }

        info!("Slot: 12, existing ledger above max size");
        let ledger_size = tick!();
        assert_eq!(ledger_size, 700);
        assert_eq!(
            watermarks.as_ref().unwrap(),
            &Watermarks {
                marks: [
                    Watermark {
                        slot: 8,
                        mod_id: 8,
                        size_delta: 400,
                    },
                    Watermark {
                        slot: 12,
                        mod_id: 12,
                        size_delta: 400,
                    },
                ]
                .into(),
                size_at_last_capture: 700,
                mark_size: 250,
                max_ledger_size: 1000,
            },
        );

        info!("Slot: 13, added 1 slot -> 800 bytes");
        ledger.add_slots(1);
        let ledger_size = tick!();
        assert_eq!(ledger_size, 800);

        info!(
            "Slot: 15, added 2 slots -> 1000 bytes -> remove estimated 400 (really 300) -> 700 bytes"
        );
        ledger.add_slots(2);
        let ledger_size = tick!();
        assert_eq!(ledger_size, 700);

        assert_eq!(
            watermarks.as_ref().unwrap(),
            &Watermarks {
                marks: [
                    Watermark {
                        slot: 12,
                        mod_id: 12,
                        size_delta: 400,
                    },
                    Watermark {
                        slot: 15,
                        mod_id: 12,
                        size_delta: 300,
                    },
                ]
                .into(),
                size_at_last_capture: 700,
                mark_size: 250,
                max_ledger_size: 1000,
            },
        );
    }

    #[tokio::test]
    async fn test_ledger_size_manager_existing_ledger_above_max_size_finality_slot_blocking_full_truncation(
    ) {
        init_logger!();

        let ledger = Arc::new(ManageableLedgerMock::new(0, 12, 12));
        let finality_provider = Arc::new(FinalityProviderMock {
            finality_slot: Mutex::new(3),
        });
        let mut watermarks = None::<Watermarks>;
        let resize_percentage = ResizePercentage::Large;
        let max_ledger_size = 1000;
        let mut existing_ledger_state = Some(ExistingLedgerState {
            size: 1200,
            slot: 12,
            mod_id: 12,
        });

        macro_rules! tick {
            () => {{
                let ledger_size = LedgerSizeManager::<
                    ManageableLedgerMock,
                    FinalityProviderMock,
                >::tick(
                    &ledger,
                    &finality_provider,
                    &mut watermarks,
                    &resize_percentage,
                    max_ledger_size,
                    &mut existing_ledger_state,
                )
                .await
                .unwrap();
                debug!("Ledger Size {} {:#?}", ledger_size, watermarks);
                ledger_size
            }};
        }

        info!("Slot: 12, existing ledger above max size");
        let ledger_size = tick!();
        // We cannot truncate above the finality slot, so we only get 300 bytes back and
        // stay above the max size
        assert_eq!(ledger_size, 900);
    }
}
