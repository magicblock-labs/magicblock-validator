use std::{
    cmp::min,
    sync::Arc,
    thread::{self, JoinHandle},
    time::Duration,
};

use log::{error, info, warn};
use solana_measure::measure::Measure;
use tokio::{runtime::Builder, time::interval};
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

struct LedgerTrunctationWorker {
    ledger: Arc<Ledger>,
    truncation_time_interval: Duration,
    ledger_size: u64,
    cancellation_token: CancellationToken,
}

impl LedgerTrunctationWorker {
    pub fn new(
        ledger: Arc<Ledger>,
        truncation_time_interval: Duration,
        ledger_size: u64,
        cancellation_token: CancellationToken,
    ) -> Self {
        Self {
            ledger,
            truncation_time_interval,
            ledger_size,
            cancellation_token,
        }
    }

    pub async fn run(self) {
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
                    if let Err(err) = self.truncate(current_size) {
                        error!("Failed to truncate ledger!: {:?}", err);
                    }
                }
            }
        }
    }

    pub fn truncate(&self, current_ledger_size: u64) -> LedgerResult<()> {
        if current_ledger_size > self.ledger_size {
            self.truncate_fat_ledger(current_ledger_size)?;
        } else {
            match self.estimate_truncation_range(current_ledger_size)? {
                Some((from_slot, to_slot)) => Self::truncate_slot_range(
                    &self.ledger,
                    from_slot,
                    to_slot,
                    self.cancellation_token.clone(),
                )?,
                None => warn!("Could not estimate truncation range"),
            }
        }

        Ok(())
    }

    /// Truncates ledger that is over desired size
    /// We rely on present `CompactionFilter` to delete all data
    pub fn truncate_fat_ledger(
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

        info!(
            "Fat truncation: truncating up to(inclusive): {}",
            truncate_to_slot
        );

        self.ledger.set_lowest_cleanup_slot(truncate_to_slot);
        Self::delete_slots(&self.ledger, 0, truncate_to_slot)?;

        if let Err(err) = self.ledger.flush() {
            // We will still compact
            error!("Failed to flush: {}", err);
        }
        Self::compact_slot_range(
            &self.ledger,
            0,
            truncate_to_slot,
            self.cancellation_token.clone(),
        );

        Ok(())
    }

    /// Inserts tombstones in slot-ordered columns for range [from; to] inclusive
    /// This is a cheap operation since delete_range inserts one range tombstone
    /// NOTE: this doesn't cover all the columns, we rely on CompactionFilter to clean the rest
    fn delete_slots(
        ledger: &Arc<Ledger>,
        from_slot: u64,
        to_slot: u64,
    ) -> LedgerResult<()> {
        let start = from_slot;
        let end = to_slot + 1;
        ledger.delete_range_cf::<Blockhash>(start, end)?;
        ledger.delete_range_cf::<Blocktime>(start, end)?;
        ledger.delete_range_cf::<PerfSamples>(start, end)?;

        // Can cheaply delete SlotSignatures as well
        // NOTE: we need to clean (to_slot, u32::MAX)
        // since range is exclusive at the end we use (to_slot + 1, 0)
        ledger.delete_range_cf::<SlotSignatures>(
            (from_slot, 0),
            (to_slot + 1, 0),
        )?;

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
        let (highest_cleanup_slot, _) = self.ledger.get_max_blockhash().ok()?;

        // Fresh start case
        let next_from_slot = if lowest_cleanup_slot == 0 {
            0
        } else {
            lowest_cleanup_slot + 1
        };

        // we don't clean latest final slot
        Some((next_from_slot, highest_cleanup_slot))
    }

    /// Utility function for splitting truncation into smaller chunks
    /// Cleans slots [from_slot; to_slot] inclusive range
    /// We rely on present `CompactionFilter` to delete all data
    pub fn truncate_slot_range(
        ledger: &Arc<Ledger>,
        from_slot: u64,
        to_slot: u64,
        cancellation_token: CancellationToken,
    ) -> LedgerResult<()> {
        if to_slot < from_slot {
            warn!("LedgerTruncator: Nani?");
            return Ok(());
        }

        info!(
            "LedgerTruncator: truncating slot range [{from_slot}; {to_slot}]"
        );

        ledger.set_lowest_cleanup_slot(to_slot);
        Self::delete_slots(ledger, from_slot, to_slot)?;

        // Flush memtables with tombstones prior to compaction
        if let Err(err) = ledger.flush() {
            error!("Failed to flush ledger: {err}");
        }
        Self::compact_slot_range(
            ledger,
            from_slot,
            to_slot,
            cancellation_token,
        );
        Ok(())
    }

    /// Synchronous utility function that triggers and awaits compaction on all the columns
    /// Compacts [from_slot; to_slot] inclusive range
    pub fn compact_slot_range(
        ledger: &Arc<Ledger>,
        from_slot: u64,
        to_slot: u64,
        cancellation_token: CancellationToken,
    ) {
        use crate::compact_cf_or_return;

        if to_slot < from_slot {
            warn!("LedgerTruncator: Nani2?");
            return;
        }
        if cancellation_token.is_cancelled() {
            info!("Validator is shutting down - skipping manual compaction");
            return;
        }

        info!(
            "LedgerTruncator: compacting slot range [{from_slot}; {to_slot}]"
        );

        struct CompactionMeasure {
            measure: Measure,
        }
        impl Drop for CompactionMeasure {
            fn drop(&mut self) {
                self.measure.stop();
                info!("Manual compaction took: {}", self.measure);
            }
        }

        let _measure = CompactionMeasure {
            measure: Measure::start("Manual compaction"),
        };

        let start = from_slot;
        let end = to_slot + 1;
        compact_cf_or_return!(
            ledger,
            cancellation_token,
            start,
            end,
            Blocktime
        );
        compact_cf_or_return!(
            ledger,
            cancellation_token,
            start,
            end,
            Blockhash
        );
        compact_cf_or_return!(
            ledger,
            cancellation_token,
            start,
            end,
            PerfSamples
        );
        compact_cf_or_return!(
            ledger,
            cancellation_token,
            (Some((from_slot, u32::MIN)), Some((to_slot + 1, 0))),
            SlotSignatures
        );
        compact_cf_or_return!(
            ledger,
            cancellation_token,
            (None, None),
            TransactionStatus
        );
        compact_cf_or_return!(
            ledger,
            cancellation_token,
            (None, None),
            Transaction
        );
        compact_cf_or_return!(
            ledger,
            cancellation_token,
            (None, None),
            TransactionMemos
        );
        compact_cf_or_return!(
            ledger,
            cancellation_token,
            (None, None),
            AddressSignatures
        );
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

pub struct LedgerTruncator {
    ledger: Arc<Ledger>,
    ledger_size: u64,
    truncation_time_interval: Duration,
    state: ServiceState,
}

impl LedgerTruncator {
    pub fn new(
        ledger: Arc<Ledger>,
        truncation_time_interval: Duration,
        ledger_size: u64,
    ) -> Self {
        Self {
            ledger,
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
                self.truncation_time_interval,
                self.ledger_size,
                cancellation_token.clone(),
            );
            let worker_handle = thread::spawn(move || {
                let runtime = Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("failed to build runtime for truncator");
                runtime.block_on(worker.run());
            });

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

    pub fn join(mut self) -> thread::Result<()> {
        if matches!(self.state, ServiceState::Running(_)) {
            self.stop();
        }

        if let ServiceState::Stopped(worker_handle) = self.state {
            worker_handle.join()
        } else {
            warn!("LedgerTruncator was not running, nothing to stop");
            Ok(())
        }
    }
}

#[macro_export]
macro_rules! compact_cf_or_return {
    ($ledger:expr, $token:expr, $from:expr, $to:expr, $cf:ty) => {{
        $ledger.compact_slot_range_cf::<$cf>(Some($from), Some($to));
        if $token.is_cancelled() {
            info!(
                "Validator shutting down - stopping compaction after {}",
                <$cf as $crate::database::columns::ColumnName>::NAME
            );
            return;
        }
    }};
    ($ledger:expr, $token:expr, ($from:expr, $to:expr), $cf:ty) => {{
        $ledger.compact_slot_range_cf::<$cf>($from, $to);
        if $token.is_cancelled() {
            info!(
                "Validator shutting down - stopping compaction after {}",
                <$cf as $crate::database::columns::ColumnName>::NAME
            );
            return;
        }
    }};
}
