use std::{cmp::min, sync::Arc, time::Duration};

use log::{error, info, warn};
use magicblock_core::traits::FinalityProvider;
use tokio::{
    task::{spawn_blocking, JoinError, JoinHandle},
    time::interval,
};
use tokio_util::sync::CancellationToken;

use crate::{
    database::columns::{
        AddressSignatures, Blockhash, Blocktime, PerfSamples, SlotSignatures,
        Transaction, TransactionMemos, TransactionStatus,
    },
    errors::LedgerResult,
    Ledger,
};

pub const DEFAULT_TRUNCATION_TIME_INTERVAL: Duration =
    Duration::from_secs(2 * 60);
const PERCENTAGE_TO_TRUNCATE: u8 = 10;
const FILLED_PERCENTAGE_LIMIT: u8 = 100 - PERCENTAGE_TO_TRUNCATE;

struct LedgerTrunctationWorker<T> {
    finality_provider: Arc<T>,
    ledger: Arc<Ledger>,
    truncation_time_interval: Duration,
    ledger_size: u64,
    cancellation_token: CancellationToken,
}

impl<T: FinalityProvider> LedgerTrunctationWorker<T> {
    pub fn new(
        ledger: Arc<Ledger>,
        finality_provider: Arc<T>,
        truncation_time_interval: Duration,
        ledger_size: u64,
        cancellation_token: CancellationToken,
    ) -> Self {
        Self {
            ledger,
            finality_provider,
            truncation_time_interval,
            ledger_size,
            cancellation_token,
        }
    }

    pub async fn run(self) {
        self.ledger
            .initialize_lowest_cleanup_slot()
            .expect("Lowest cleanup slot initialization");
        let mut interval = interval(self.truncation_time_interval);
        loop {
            tokio::select! {
                _ = self.cancellation_token.cancelled() => {
                    return;
                }
                _ = interval.tick() => {
                    // Note: since we clean 10%, tomstones will take around 10% as well

                    let current_size = match self.ledger.storage_size() {
                        Ok(value) => value,
                        Err(err) => {
                            error!("Failed to check truncation condition: {err}");
                            continue;
                        }
                    };

                    // Check if we should truncate
                    if current_size < (self.ledger_size / 100) * FILLED_PERCENTAGE_LIMIT as u64 {
                        info!("Skipping truncation, ledger size: {}", current_size);
                        continue;
                    }

                    info!("Ledger size: {current_size}");
                    if let Err(err) = self.truncate(current_size).await {
                        error!("Failed to truncate ledger!: {:?}", err);
                    }
                }
            }
        }
    }

    pub async fn truncate(&self, current_ledger_size: u64) -> LedgerResult<()> {
        if current_ledger_size > self.ledger_size {
            self.truncate_fat_ledger(current_ledger_size).await?;
        } else {
            match self.estimate_truncation_range(current_ledger_size)? {
                Some((from_slot, to_slot)) => {
                    Self::truncate_slot_range(&self.ledger, from_slot, to_slot)
                        .await
                }
                None => warn!("Could not estimate truncation range"),
            }
        }

        Ok(())
    }

    /// Truncates ledger that is over desired size
    pub async fn truncate_fat_ledger(
        &self,
        current_ledger_size: u64,
    ) -> LedgerResult<()> {
        info!("Fat truncation");

        // Calculate excessive size
        let desired_size =
            (self.ledger_size / 100) * FILLED_PERCENTAGE_LIMIT as u64;
        let excess = current_ledger_size - desired_size;

        let (highest_slot, _) = self.ledger.get_max_blockhash()?;
        let lowest_slot = self.ledger.get_lowest_slot()?.unwrap_or(0);
        info!(
            "Fat truncation. lowest slot: {}, hightest slot: {}",
            lowest_slot, highest_slot
        );
        if lowest_slot == highest_slot {
            warn!("Nani3?");
            return Ok(());
        }

        // Estimating number of slot we need to truncating
        let num_slots = highest_slot - lowest_slot + 1;
        let slot_size = current_ledger_size / num_slots;
        let num_slots_to_truncate = excess / slot_size;

        // Calculating up to which slot we're truncating
        let truncate_to_slot = lowest_slot + num_slots_to_truncate - 1;
        let finality_slot = self.finality_provider.get_latest_final_slot();
        let truncate_to_slot = if truncate_to_slot >= finality_slot {
            // Shouldn't really happen
            warn!("LedgerTruncator: want to truncate past finality slot, finality slot:{}, truncating to: {}", finality_slot, truncate_to_slot);
            if finality_slot == 0 {
                // No truncation at that case
                return Ok(());
            } else {
                // Not cleaning finality slot
                finality_slot - 1
            }
        } else {
            truncate_to_slot
        };

        info!(
            "Fat truncation: truncating up to(inclusive): {}",
            truncate_to_slot
        );
        self.ledger.set_lowest_cleanup_slot(truncate_to_slot);
        if let Err(err) = self.ledger.flush() {
            // We will still compact
            error!("Failed to flush: {}", err);
        }
        Self::compact_slot_range(&self.ledger, 0, truncate_to_slot).await;

        Ok(())
    }

    /// Returns range to truncate [from_slot, to_slot]
    fn estimate_truncation_range(
        &self,
        current_ledger_size: u64,
    ) -> LedgerResult<Option<(u64, u64)>> {
        let (from_slot, to_slot) =
            if let Some(val) = self.available_truncation_range() {
                val
            } else {
                return Ok(None);
            };

        let num_slots = self.ledger.count_blockhashes()?;
        if num_slots == 0 {
            info!("No slot were written yet. Nothing to truncate!");
            return Ok(None);
        }

        let slot_size = current_ledger_size / num_slots as u64;
        let size_to_truncate =
            (current_ledger_size / 100) * PERCENTAGE_TO_TRUNCATE as u64;
        let num_slots_to_truncate = size_to_truncate / slot_size;

        let to_slot = min(from_slot + num_slots_to_truncate, to_slot);
        Ok(Some((from_slot, to_slot)))
    }

    /// Returns [from_slot, to_slot] range that's safe to truncate
    fn available_truncation_range(&self) -> Option<(u64, u64)> {
        let lowest_cleanup_slot = self.ledger.get_lowest_cleanup_slot();
        let latest_final_slot = self.finality_provider.get_latest_final_slot();

        if latest_final_slot <= lowest_cleanup_slot {
            // Could both be 0 at startup, no need to report
            if lowest_cleanup_slot != 0 {
                // This could not happen because of Truncator
                warn!("Slots after latest final slot have been truncated!");
            }

            info!(
                "Lowest cleanup slot ge than latest final slot. {}, {}",
                lowest_cleanup_slot, latest_final_slot
            );
            return None;
        }
        // Nothing to truncate
        if latest_final_slot == lowest_cleanup_slot + 1 {
            info!("Nothing to truncate");
            return None;
        }

        // Fresh start case
        let next_from_slot = if lowest_cleanup_slot == 0 {
            0
        } else {
            lowest_cleanup_slot + 1
        };

        // we don't clean latest final slot
        Some((next_from_slot, latest_final_slot - 1))
    }

    /// Utility function for splitting truncation into smaller chunks
    /// Cleans slots [from_slot; to_slot] inclusive range
    pub async fn truncate_slot_range(
        ledger: &Arc<Ledger>,
        from_slot: u64,
        to_slot: u64,
    ) {
        // In order not to torture RocksDB's WriteBatch we split large tasks into chunks
        const SINGLE_TRUNCATION_LIMIT: usize = 300;

        if to_slot < from_slot {
            warn!("LedgerTruncator: Nani?");
            return;
        }

        info!(
            "LedgerTruncator: truncating slot range [{from_slot}; {to_slot}]"
        );
        (from_slot..=to_slot)
            .step_by(SINGLE_TRUNCATION_LIMIT)
            .for_each(|cur_from_slot| {
                let num_slots_to_truncate = min(
                    to_slot - cur_from_slot + 1,
                    SINGLE_TRUNCATION_LIMIT as u64,
                );
                let truncate_to_slot =
                    cur_from_slot + num_slots_to_truncate - 1;

                if let Err(err) =
                    ledger.delete_slot_range(cur_from_slot, truncate_to_slot)
                {
                    warn!(
                        "Failed to truncate slots {}-{}: {}",
                        cur_from_slot, truncate_to_slot, err
                    );
                }
            });
        // Flush memtables with tombstones prior to compaction
        if let Err(err) = ledger.flush() {
            error!("Failed to flush ledger: {err}");
        }

        Self::compact_slot_range(ledger, from_slot, to_slot).await;
    }

    /// Synchronous utility function that triggers and awaits compaction on all the columns
    /// Compacts [from_slot; to_slot] inclusing ramge
    pub async fn compact_slot_range(
        ledger: &Arc<Ledger>,
        from_slot: u64,
        to_slot: u64,
    ) {
        if to_slot < from_slot {
            warn!("LedgerTruncator: Nani2?");
            return;
        }

        // Compaction can be run concurrently for different cf
        // but it utilizes rocksdb threads, in order not to drain
        // our tokio rt threads, we split the effort in just 3 tasks
        let ledger_copy = ledger.clone();
        let handler = spawn_blocking(move || {
            ledger_copy.compact_slot_range_cf::<Blocktime>(
                Some(from_slot),
                Some(to_slot + 1),
            );
            ledger_copy.compact_slot_range_cf::<Blockhash>(
                Some(from_slot),
                Some(to_slot + 1),
            );
            ledger_copy.compact_slot_range_cf::<PerfSamples>(
                Some(from_slot),
                Some(to_slot + 1),
            );
            ledger_copy.compact_slot_range_cf::<SlotSignatures>(
                Some((from_slot, u32::MIN)),
                Some((to_slot + 1, u32::MAX)),
            );

            ledger_copy.compact_slot_range_cf::<TransactionStatus>(None, None);
            ledger_copy.compact_slot_range_cf::<Transaction>(None, None);
            ledger_copy.compact_slot_range_cf::<TransactionMemos>(None, None);
            ledger_copy.compact_slot_range_cf::<AddressSignatures>(None, None);
        });
        if let Err(err) = handler.await {
            error!("compaction aborted {}", err);
        }
    }
}

#[derive(Debug)]
struct WorkerController {
    cancellation_token: CancellationToken,
    worker_handle: JoinHandle<()>,
}

#[derive(Debug)]
enum ServiceState {
    Created,
    Running(WorkerController),
    Stopped(JoinHandle<()>),
}

pub struct LedgerTruncator<T> {
    finality_provider: Arc<T>,
    ledger: Arc<Ledger>,
    ledger_size: u64,
    truncation_time_interval: Duration,
    state: ServiceState,
}

impl<T: FinalityProvider> LedgerTruncator<T> {
    pub fn new(
        ledger: Arc<Ledger>,
        finality_provider: Arc<T>,
        truncation_time_interval: Duration,
        ledger_size: u64,
    ) -> Self {
        Self {
            ledger,
            finality_provider,
            truncation_time_interval,
            ledger_size,
            state: ServiceState::Created,
        }
    }

    pub fn start(&mut self) {
        if let ServiceState::Created = self.state {
            let cancellation_token = CancellationToken::new();
            let worker = LedgerTrunctationWorker::new(
                self.ledger.clone(),
                self.finality_provider.clone(),
                self.truncation_time_interval,
                self.ledger_size,
                cancellation_token.clone(),
            );
            let worker_handle = tokio::spawn(worker.run());

            self.state = ServiceState::Running(WorkerController {
                cancellation_token,
                worker_handle,
            })
        } else {
            warn!("LedgerTruncator already running, no need to start.");
        }
    }

    pub fn stop(&mut self) {
        let state = std::mem::replace(&mut self.state, ServiceState::Created);
        if let ServiceState::Running(controller) = state {
            controller.cancellation_token.cancel();
            self.state = ServiceState::Stopped(controller.worker_handle);
        } else {
            warn!("LedgerTruncator not running, can not be stopped.");
            self.state = state;
        }
    }

    pub async fn join(mut self) -> Result<(), LedgerTruncatorError> {
        if matches!(self.state, ServiceState::Running(_)) {
            self.stop();
        }

        if let ServiceState::Stopped(worker_handle) = self.state {
            worker_handle.await?;
            Ok(())
        } else {
            warn!("LedgerTruncator was not running, nothing to stop");
            Ok(())
        }
    }
}

#[derive(thiserror::Error, Debug)]
pub enum LedgerTruncatorError {
    #[error("Failed to join worker: {0}")]
    JoinError(#[from] JoinError),
}
