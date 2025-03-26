use std::{cmp::min, sync::Arc, time::Duration};

use anyhow::Context;
use log::{error, warn};
use tokio::{task::JoinHandle, time::interval};
use tokio_util::sync::CancellationToken;

use crate::Ledger;

pub const DEFAULT_PURGE_TIME_INTERVAL: Duration = Duration::from_secs(10 * 60);

/// Provides slot after which it is safe to purge slots
/// At the moment it depends on latest snapshot slot
/// but it may change in the future
pub trait FinalityProvider: Send + Clone + 'static {
    fn get_latest_final_slot(&self) -> u64;
}

struct LedgerPurgatoryWorker<T> {
    finality_provider: T,
    ledger: Arc<Ledger>,
    slot_purge_interval: u64, // TODO: mauybe rename to slots_preserved/extra_slots_preserved
    purge_time_interval: Duration,
    size_thresholds_bytes: u64, // TODO: rename to: report_size
    cancellation_token: CancellationToken,
}

impl<T: FinalityProvider> LedgerPurgatoryWorker<T> {
    pub fn new(
        ledger: Arc<Ledger>,
        finality_provider: T,
        slot_purge_interval: u64,
        purge_time_interval: Duration,
        size_thresholds_bytes: u64,
        cancellation_token: CancellationToken,
    ) -> Self {
        Self {
            ledger,
            finality_provider,
            slot_purge_interval,
            purge_time_interval,
            size_thresholds_bytes,
            cancellation_token,
        }
    }

    pub async fn run(self) {
        let mut interval = interval(self.purge_time_interval);
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

                    // TODO: hold lock for the whole duration?
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
        if latest_final_slot <= lowest_cleanup_slot {
            // This could not happen because of Purgatory
            warn!("Slots after latest final slot have been purged!");
            return None;
        }

        let next_from_slot = if lowest_cleanup_slot == 0 {
            0
        } else {
            lowest_cleanup_slot + 1
        };

        // The idea here is that slot_purge_interval number of slots
        // prior to latest_final_slot are preserved as well
        if latest_final_slot - next_from_slot <= self.slot_purge_interval {
            None
        } else {
            // Always positive since latest_final_slot > self.slot_purge_interval
            let to_slot = latest_final_slot - self.slot_purge_interval - 1;
            Some((next_from_slot, to_slot))
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
    worker_handle: JoinHandle<()>,
}

enum ServiceState {
    Created,
    Running(WorkerController),
    Stopped(JoinHandle<()>),
}

pub struct LedgerPurgatory<T> {
    finality_provider: T,
    ledger: Arc<Ledger>,
    size_thresholds_bytes: u64,
    purge_time_interval: Duration,
    slot_purge_interval: u64,
    state: ServiceState,
}

impl<T: FinalityProvider> LedgerPurgatory<T> {
    pub fn new(
        ledger: Arc<Ledger>,
        finality_provider: T,
        slot_purge_interval: u64,
        purge_time_interval: Duration,
        size_thresholds_bytes: u64,
    ) -> Self {
        Self {
            ledger,
            finality_provider,
            slot_purge_interval,
            purge_time_interval,
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
                self.purge_time_interval,
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

    pub fn stop(&mut self) {
        let state = std::mem::replace(&mut self.state, ServiceState::Created);
        if let ServiceState::Running(controller) = state {
            controller.cancellation_token.cancel();
            self.state = ServiceState::Stopped(controller.worker_handle);
        } else {
            warn!("LedgerPurgatory not running, can not be stopped.");
            self.state = state;
        }
    }

    pub async fn join(mut self) -> Result<(), anyhow::Error> {
        if matches!(self.state, ServiceState::Running(_)) {
            self.stop();
        }

        if let ServiceState::Stopped(worker_handle) = self.state {
            worker_handle.await.context("Failed to join worker")?;
            Ok(())
        } else {
            warn!("Purgatory was not running, nothing to stop");
            Ok(())
        }
    }
}
