use std::{cmp::min, sync::Arc, time::Duration};

use log::{error, warn};
use tokio::{task::JoinHandle, time::interval};
use tokio_util::sync::CancellationToken;

use crate::Ledger;

/// Provides slot after which it is safe to purge slots
/// At the moment it depends on latest snapshot slot
/// but it may change in the future
pub trait FinalityProvider: Send + Clone + 'static {
    fn get_latest_final_slot(&self) -> u64;
}

struct LedgerPurgatoryWorker<T> {
    finality_provider: T,
    ledger: Arc<Ledger>,
    slot_purge_interval: u64,
    size_thresholds_bytes: u64,
    cancellation_token: CancellationToken,
}

impl<T: FinalityProvider> LedgerPurgatoryWorker<T> {
    const PURGE_TIME_INTERVAL: Duration = Duration::from_secs(10 * 60);

    pub fn new(
        ledger: Arc<Ledger>,
        finality_provider: T,
        slot_purge_interval: u64,
        size_thresholds_bytes: u64,
        cancellation_token: CancellationToken,
    ) -> Self {
        Self {
            ledger,
            finality_provider,
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
        let latest_final_slot = self.finality_provider.get_latest_final_slot();

        if latest_final_slot - lowest_cleanup_slot > self.slot_purge_interval {
            let to_slot = latest_final_slot - self.slot_purge_interval;
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

pub struct LedgerPurgatory<T> {
    finality_provider: T,
    ledger: Arc<Ledger>,
    size_thresholds_bytes: u64,
    slot_purge_interval: u64,
    state: ServiceState,
}

impl<T: FinalityProvider> LedgerPurgatory<T> {
    pub fn new(
        ledger: Arc<Ledger>,
        finality_provider: T,
        slot_purge_interval: u64,
        size_thresholds_bytes: u64,
    ) -> Self {
        Self {
            ledger,
            finality_provider,
            slot_purge_interval,
            size_thresholds_bytes,
            state: ServiceState::Created,
        }
    }

    pub fn start(&mut self) {
        if let ServiceState::Created = self.state {
            let cancellation_token = CancellationToken::new();
            let worker = LedgerPurgatoryWorker::new(
                self.ledger.clone(),
                self.finality_provider.clone(),
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
