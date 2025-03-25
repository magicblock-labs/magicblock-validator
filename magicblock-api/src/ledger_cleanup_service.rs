use std::{cmp::min, mem::take, num::NonZero, sync::Arc, time::Duration};

use log::{error, warn};
use magicblock_bank::bank::Bank;
use magicblock_ledger::{errors::LedgerError, Ledger};
use tokio::{task::JoinHandle, time::interval};
use tokio_util::sync::CancellationToken;

struct WorkerController {
    cancellation_token: CancellationToken,
    worker_handle: JoinHandle<()>, // TODO: probably not necessary
}

enum ServiceState {
    Created,
    Running(WorkerController),
    // Stopped
}

struct LedgerCleanupService {
    ledger: Arc<Ledger>,
    bank: Arc<Bank>,
    size_thresholds_bytes: u64,
    slot_purge_interval: u64,
    state: ServiceState,
}

impl LedgerCleanupService {
    // TODO: probably move to config
    const CLEANUP_INTERVAL: Duration = Duration::from_secs(10 * 60);

    pub fn new(
        ledger: Arc<Ledger>,
        bank: Arc<Bank>,
        slot_purge_interval: u64,
        size_thresholds_bytes: u64,
    ) -> Self {
        // Our slot purge interval is connected to snapshot frequency
        // Slots will be cleaned after
        // Bank::get_latest_snapshot_slot() -
        // const NUM_SNAPSHOTS: u64 = 2;
        // let slot_purge_interval = NUM_SNAPSHOTS * snapshot_frequency;
        // With this max size will be
        // upperbound of stored slots: (NUM_SNAPSHOTS+1)*snapshot_frequency - 1
        // TPS: 50000 tx/s, max tx size - 1232 bytes
        // slot: every 50 ms
        // (3*1024 - 1)*2500*1232 = 9,45 GB

        Self {
            ledger,
            bank,
            slot_purge_interval,
            size_thresholds_bytes,
            state: ServiceState::Created,
        }
    }

    pub fn start(&mut self) {
        if let ServiceState::Created = self.state {
            let cancellation_token = CancellationToken::new();
            let worker_handle = tokio::spawn(Self::worker(
                self.ledger.clone(),
                self.bank.clone(),
                self.slot_purge_interval,
                self.size_thresholds_bytes,
                cancellation_token.clone(),
            ));

            self.state = ServiceState::Running(WorkerController {
                cancellation_token,
                worker_handle,
            })
        } else {
            warn!("LedgerCleanupService already running, no need to start.");
        }
    }

    pub fn stop(&self) {
        if let ServiceState::Running(ref controller) = self.state {
            controller.cancellation_token.cancel();
        } else {
            warn!("LedgerCleanupService not running, can not be stopped.");
        }
    }

    pub async fn join(self) {
        if let ServiceState::Running(controller) = self.state {
            let _ = controller.worker_handle.await.inspect_err(|err| {
                error!("LedgerCleanupService exited with error: {err}")
            });
        }
    }

    pub async fn worker(
        ledger: Arc<Ledger>,
        bank: Arc<Bank>,
        slot_purge_interval: u64,
        size_thresholds_bytes: u64,
        cancellation_token: CancellationToken,
    ) {
        let mut interval = interval(Self::CLEANUP_INTERVAL);
        loop {
            tokio::select! {
                _ = cancellation_token.cancelled() => {
                    return;
                }
                _ = interval.tick() => {
                    match ledger.storage_size() {
                        Ok(size) => {
                            // If this happens whether unreasonable size_thresholds_bytes
                            // or slot_purge_interval were chosen
                            if size > size_thresholds_bytes {
                                warn!("Ledger size threshold exceeded.");
                            }
                        }
                        Err(err) =>  {
                            error!("Failed to fetch ledger size: {err}");
                        }
                    }

                    if let Some((from_slot, to_slot)) = Self::next_cleanup_range(&ledger, &bank, slot_purge_interval) {
                        let let Err(err) = Self::cleanup(&ledger, from_slot, to_slot) {
                            error!("Failed to cleanup: {err}");
                        };
                    }
                }
            }
        }
    }

    /// Returns next cleanup range if cleanup required
    fn next_cleanup_range(
        ledger: &Arc<Ledger>,
        bank: &Arc<Bank>,
        slot_purge_interval: u64,
    ) -> Option<(u64, u64)> {
        let lowest_cleanup_slot = ledger.get_lowest_cleanup_slot();
        let latest_snapshot_slot = bank.get_latest_snapshot_slot();

        if latest_snapshot_slot - lowest_cleanup_slot > slot_purge_interval {
            let to_slot = latest_snapshot_slot - slot_purge_interval;
            Some((lowest_cleanup_slot, to_slot))
        } else {
            None
        }
    }

    fn cleanup(ledger: &Arc<Ledger>, from_slot: u64, to_slot: u64) {
        // In order not RocksDB's WriteBatch we split large tasks into chunks
        const SINGLE_PURGE_LIMIT: u64 = 3000;

        if to_slot < from_slot {
            warn!("LedgerCleanupService: Nani?");
            return;
        }

        // Since [from_slot; to_slot] is inclusive
        let num_to_purge = to_slot - from_slot + 1;
        let num_of_purges =
            (num_to_purge + SINGLE_PURGE_LIMIT - 1) / SINGLE_PURGE_LIMIT;

        (0..num_of_purges).fold(from_slot, |cur_from_slot, el| {
            let num_slots_to_purge =
                min(to_slot - cur_from_slot + 1, SINGLE_PURGE_LIMIT);
            let to_slot = cur_from_slot + num_slots_to_purge - 1;

            // This is critical error, but since otherwise we will get stuck,
            // we report & continue
            if let Err(err) = ledger.purge_slots(cur_from_slot, to_slot) {
                error!(
                    "Failed to purge Ledger, slot interval from: {}, to: {}. {}",
                    cur_from_slot, to_slot, err
                )
            };

            cur_from_slot + SINGLE_PURGE_LIMIT
        });
    }
}
