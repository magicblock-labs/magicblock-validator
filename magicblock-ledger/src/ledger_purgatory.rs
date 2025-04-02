use std::{cmp::min, sync::Arc, time::Duration};

use anyhow::Context;
use log::{error, info, warn};
use magicblock_core::traits::FinalityProvider;
use tokio::{task::JoinHandle, time::interval};
use tokio_util::sync::CancellationToken;

use crate::{errors::LedgerResult, Ledger};

pub const DEFAULT_PURGE_TIME_INTERVAL: Duration = Duration::from_secs(10 * 60);

struct LedgerPurgatoryWorker<T> {
    finality_provider: Arc<T>,
    ledger: Arc<Ledger>,
    slots_to_preserve: u64,
    purge_time_interval: Duration,
    desired_size: u64,
    cancellation_token: CancellationToken,
}

impl<T: FinalityProvider> LedgerPurgatoryWorker<T> {
    pub fn new(
        ledger: Arc<Ledger>,
        finality_provider: Arc<T>,
        slots_to_preserve: u64,
        purge_time_interval: Duration,
        desired_size: u64,
        cancellation_token: CancellationToken,
    ) -> Self {
        Self {
            ledger,
            finality_provider,
            slots_to_preserve,
            purge_time_interval,
            desired_size,
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
                    match self.should_purge() {
                        Ok(true) => {
                            if let Some((from_slot, to_slot)) = self.next_purge_range() {
                                info!("Purging slots [{from_slot};{to_slot}]");
                                Self::purge(&self.ledger, from_slot, to_slot);
                            } else {
                                warn!("Failed to get purging range! Ledger size exceeded desired threshold");
                            }
                        },
                        Ok(false) => (),
                        Err(err) => error!("Failed to check purge condition: {err}"),
                    }
                }
            }
        }
    }

    fn should_purge(&self) -> LedgerResult<bool> {
        Ok(self.ledger.storage_size()? > self.desired_size)
    }

    /// Returns [from_slot, to_slot] range that's safe to purge
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

        // The idea here is that slots_to_preserve number of slots
        // prior to latest_final_slot are preserved as well
        if latest_final_slot - next_from_slot <= self.slots_to_preserve {
            None
        } else {
            // Always positive since latest_final_slot > self.slots_to_preserve
            let to_slot = latest_final_slot - self.slots_to_preserve - 1;
            Some((next_from_slot, to_slot))
        }
    }

    /// Utility function for splitting purging into smaller chunks
    /// Cleans slots [from_slot; to_slot] inclusive range
    pub fn purge(ledger: &Arc<Ledger>, from_slot: u64, to_slot: u64) {
        // In order not to torture RocksDB's WriteBatch we split large tasks into chunks
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

pub struct LedgerPurgatory<T> {
    finality_provider: Arc<T>,
    ledger: Arc<Ledger>,
    desired_size: u64,
    purge_time_interval: Duration,
    slots_to_preserve: u64,
    state: ServiceState,
}

impl<T: FinalityProvider> LedgerPurgatory<T> {
    pub fn new(
        ledger: Arc<Ledger>,
        finality_provider: Arc<T>,
        slots_to_preserve: u64,
        purge_time_interval: Duration,
        desired_size: u64,
    ) -> Self {
        Self {
            ledger,
            finality_provider,
            slots_to_preserve,
            purge_time_interval,
            desired_size,
            state: ServiceState::Created,
        }
    }

    pub fn start(&mut self) {
        if let ServiceState::Created = self.state {
            let cancellation_token = CancellationToken::new();
            let worker = LedgerPurgatoryWorker::new(
                self.ledger.clone(),
                self.finality_provider.clone(),
                self.slots_to_preserve,
                self.purge_time_interval,
                self.desired_size,
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
