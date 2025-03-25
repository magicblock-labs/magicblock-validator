use std::{
    cmp::{max, min},
    sync::Arc,
    time::Duration,
};

use anyhow::anyhow;
use log::{error, warn};
use magicblock_accounts_db::config::AccountsDbConfig;
use magicblock_bank::bank::Bank;
use magicblock_config::EphemeralConfig;
use magicblock_ledger::{errors::LedgerError, Ledger};
use tokio::{task::JoinHandle, time::interval};
use tokio_util::sync::CancellationToken;

struct LedgerPurgatoryWorker {
    ledger: Arc<Ledger>,
    bank: Arc<Bank>,
    slot_purge_interval: u64,
    size_thresholds_bytes: u64,
    cancellation_token: CancellationToken,
}

impl LedgerPurgatoryWorker {
    // TODO: probably move to config
    const PURGE_TIME_INTERVAL: Duration = Duration::from_secs(10 * 60);

    pub fn new(
        ledger: Arc<Ledger>,
        bank: Arc<Bank>,
        slot_purge_interval: u64,
        size_thresholds_bytes: u64,
        cancellation_token: CancellationToken,
    ) -> Self {
        Self {
            ledger,
            bank,
            slot_purge_interval,
            size_thresholds_bytes,
            cancellation_token,
        }
    }

    pub async fn run(self) {
        let mut interval = interval(Self::PURGE_TIME_INTERVAL);
        loop {
            tokio::select! {
                _ = self.cancellation_token.cancelled() => {
                    return;
                }
                _ = interval.tick() => {
                    match self.ledger.storage_size() {
                        Ok(size) => {
                            // If this happens whether unreasonable size_thresholds_bytes
                            // or slot_purge_interval were chosen
                            if size > self.size_thresholds_bytes {
                                warn!("Ledger size threshold exceeded.");
                            }
                        }
                        Err(err) =>  {
                            error!("Failed to fetch ledger size: {err}");
                        }
                    }

                    if let Some((from_slot, to_slot)) = self.next_purge_range() {
                        Self::purge(&self.ledger, from_slot, to_slot);
                    }
                }
            }
        }
    }

    /// Returns next purge range if purge required
    fn next_purge_range(&self) -> Option<(u64, u64)> {
        let lowest_cleanup_slot = self.ledger.get_lowest_cleanup_slot();
        let latest_snapshot_slot = self.bank.get_latest_snapshot_slot();

        if latest_snapshot_slot - lowest_cleanup_slot > self.slot_purge_interval
        {
            let to_slot = latest_snapshot_slot - self.slot_purge_interval;
            Some((lowest_cleanup_slot, to_slot))
        } else {
            None
        }
    }

    /// Utility function for splitting purging into smaller chunks
    /// Cleans slots [from_slot; to_slot] inclusive range
    pub fn purge(ledger: &Arc<Ledger>, from_slot: u64, to_slot: u64) {
        // In order not torture RocksDB's WriteBatch we split large tasks into chunks
        const SINGLE_PURGE_LIMIT: usize = 3000;

        if to_slot < from_slot {
            warn!("LedgerPurgatory: Nani?");
            return;
        }
        (from_slot..=to_slot).step_by(SINGLE_PURGE_LIMIT).for_each(
            |cur_from_slot| {
                let num_slots_to_purge =
                    min(to_slot - cur_from_slot + 1, SINGLE_PURGE_LIMIT as u64);
                let purge_to_slot = cur_from_slot + num_slots_to_purge - 1;

                if let Err(err) =
                    ledger.purge_slots(cur_from_slot, purge_to_slot)
                {
                    error!(
                        "Failed to purge slots {}-{}: {}",
                        cur_from_slot, purge_to_slot, err
                    );
                }
            },
        );
    }
}

struct WorkerController {
    cancellation_token: CancellationToken,
    worker_handle: JoinHandle<()>, // TODO: probably not necessary
}

enum ServiceState {
    Created,
    Running(WorkerController),
}

pub struct LedgerPurgatory {
    ledger: Arc<Ledger>,
    bank: Arc<Bank>,
    size_thresholds_bytes: u64,
    slot_purge_interval: u64,
    state: ServiceState,
}

impl LedgerPurgatory {
    pub fn new(
        ledger: Arc<Ledger>,
        bank: Arc<Bank>,
        slot_purge_interval: u64,
        size_thresholds_bytes: u64,
    ) -> Self {
        Self {
            ledger,
            bank,
            slot_purge_interval,
            size_thresholds_bytes,
            state: ServiceState::Created,
        }
    }

    pub fn from_config(
        ledger: Arc<Ledger>,
        bank: Arc<Bank>,
        config: &EphemeralConfig,
    ) -> Self {
        // We terminate in case of invalid config
        let purge_slot_interval = Self::estimate_purge_slot_interval(config)
            .expect("Failed to estimate purge slot interval");
        Self::new(
            ledger,
            bank,
            purge_slot_interval,
            config.ledger.desired_size,
        )
    }

    /// Calculates how many slots shall pass by for next truncation to happen
    /// We make some assumption here on TPS & size per transaction
    pub fn estimate_purge_slot_interval(
        config: &EphemeralConfig,
    ) -> Result<u64, anyhow::Error> {
        // Could be dynamic in the future and fetched from stats.
        const TRANSACTIONS_PER_SECOND: u64 = 50000;
        // Some of the info is duplicated over columns, but mostly a negligible amount
        // So we take solana max transaction size
        const TRANSACTION_MAX_SIZE: u64 = 1232;
        // This implies that we can't delete ledger data past
        // latest MIN_SNAPSHOTS_KEPT snapshot. Has to be at least 1
        const MIN_SNAPSHOTS_KEPT: u16 = 2;
        // with 50 ms slots & 1024 default snapshot frequency
        // 7*1024*2500*1232 ~ 22 GiB of data in ledger
        const MAX_SNAPSHOTS_KEPT: u16 = 7;

        let millis_per_slot = config.validator.millis_per_slot;
        let transactions_per_slot =
            (millis_per_slot * TRANSACTIONS_PER_SECOND) / 1000;
        let size_per_slot = transactions_per_slot * TRANSACTION_MAX_SIZE;

        let AccountsDbConfig {
            max_snapshots,
            snapshot_frequency,
            ..
        } = &config.accounts.db;
        let desired_size = config.ledger.desired_size;

        // Calculate how many snapshot it will take to exceed desired size
        let slots_size =
            snapshot_frequency
                .checked_mul(size_per_slot)
                .ok_or(anyhow!(
                    "slot_size overflowed. snapshot frequency is too large"
                ))?;

        let num_snapshots_in_desired_size =
            desired_size.checked_div(slots_size).ok_or(anyhow!(
                "Failed to calculate num_snapshots_in_desired_size"
            ))?;

        // Take min of 2
        let upper_bound_snapshots_kept =
            min(MAX_SNAPSHOTS_KEPT, *max_snapshots);
        let snapshots_kept = min(
            upper_bound_snapshots_kept as u64,
            num_snapshots_in_desired_size,
        ) as u16;

        if snapshots_kept < MIN_SNAPSHOTS_KEPT {
            Err(anyhow!("Desired ledger size is too small. Required snapshots to keep: {}, got: {}", MIN_SNAPSHOTS_KEPT, snapshots_kept))
        } else {
            Ok(snapshots_kept as u64 * snapshot_frequency)
        }
    }

    pub fn start(&mut self) {
        if let ServiceState::Created = self.state {
            let cancellation_token = CancellationToken::new();
            let worker = LedgerPurgatoryWorker::new(
                self.ledger.clone(),
                self.bank.clone(),
                self.slot_purge_interval,
                self.size_thresholds_bytes,
                cancellation_token.clone(),
            );
            let worker_handle = tokio::spawn(worker.run());

            self.state = ServiceState::Running(WorkerController {
                cancellation_token,
                worker_handle,
            })
        } else {
            warn!("LedgerPurgatory already running, no need to start.");
        }
    }

    pub fn stop(&self) {
        if let ServiceState::Running(ref controller) = self.state {
            controller.cancellation_token.cancel();
        } else {
            warn!("LedgerPurgatory not running, can not be stopped.");
        }
    }

    pub async fn join(self) {
        if let ServiceState::Running(controller) = self.state {
            if let Err(err) = controller.worker_handle.await {
                error!("LedgerPurgatory exited with error: {err}")
            };
        }
    }
}
