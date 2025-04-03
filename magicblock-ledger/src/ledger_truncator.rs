use std::{cmp::min, ops::ControlFlow, sync::Arc, time::Duration};

use anyhow::Context;
use log::{error, warn};
use magicblock_core::traits::FinalityProvider;
use tokio::{task::JoinHandle, time::interval};
use tokio_util::sync::CancellationToken;

use crate::{errors::LedgerResult, Ledger};

pub const DEFAULT_TRUNCATION_TIME_INTERVAL: Duration =
    Duration::from_secs(10 * 60);

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
        let mut interval = interval(self.truncation_time_interval);
        loop {
            tokio::select! {
                _ = self.cancellation_token.cancelled() => {
                    return;
                }
                _ = interval.tick() => {
                    const TRUNCATE_TO_PERCENTAGE: u64 = 90;

                    match self.should_truncate() {
                        Ok(true) => {
                            if let Some((from_slot, to_slot)) = self.next_truncation_range() {
                                let to_size = ( self.ledger_size / 100 ) * TRUNCATE_TO_PERCENTAGE;
                                Self::truncate_to_size(&self.ledger, to_size, from_slot, to_slot);
                            } else {
                                warn!("Failed to get truncation range! Ledger size exceeded desired threshold");
                            }
                        },
                        Ok(false) => (),
                        Err(err) => error!("Failed to check truncation condition: {err}"),
                    }
                }
            }
        }
    }

    fn should_truncate(&self) -> LedgerResult<bool> {
        // Once size percentage reached, we start truncation
        const FILLED_PERCENTAGE_LIMIT: u64 = 98;
        Ok(self.ledger.storage_size()?
            >= (self.ledger_size / 100) * FILLED_PERCENTAGE_LIMIT)
    }

    /// Returns [from_slot, to_slot] range that's safe to truncate
    fn next_truncation_range(&self) -> Option<(u64, u64)> {
        let lowest_cleanup_slot = self.ledger.get_lowest_cleanup_slot();
        let latest_final_slot = self.finality_provider.get_latest_final_slot();

        if latest_final_slot <= lowest_cleanup_slot {
            // Could both be 0 at startup, no need to report
            if lowest_cleanup_slot != 0 {
                // This could not happen because of Truncator
                warn!("Slots after latest final slot have been truncated!");
            }
            return None;
        }
        // Nothing to clean
        if latest_final_slot - 1 == lowest_cleanup_slot {
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
    pub fn truncate_to_size(
        ledger: &Arc<Ledger>,
        size: u64,
        from_slot: u64,
        to_slot: u64,
    ) {
        // In order not to torture RocksDB's WriteBatch we split large tasks into chunks
        const SINGLE_TRUNCATION_LIMIT: usize = 300;

        if to_slot < from_slot {
            warn!("LedgerTruncator: Nani?");
            return;
        }
        (from_slot..=to_slot)
            .step_by(SINGLE_TRUNCATION_LIMIT)
            .try_for_each(|cur_from_slot| {
                let num_slots_to_truncate = min(
                    to_slot - cur_from_slot + 1,
                    SINGLE_TRUNCATION_LIMIT as u64,
                );
                let truncate_to_slot =
                    cur_from_slot + num_slots_to_truncate - 1;

                if let Err(err) =
                    ledger.truncate_slots(cur_from_slot, truncate_to_slot)
                {
                    warn!(
                        "Failed to truncate slots {}-{}: {}",
                        cur_from_slot, truncate_to_slot, err
                    );

                    return ControlFlow::Continue(());
                }

                match ledger.storage_size() {
                    Ok(current_size) => {
                        if current_size <= size {
                            ControlFlow::Break(())
                        } else {
                            ControlFlow::Continue(())
                        }
                    }
                    Err(err) => {
                        warn!("Failed to fetch Ledger size: {err}");
                        ControlFlow::Continue(())
                    }
                }
            });
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

    pub async fn join(mut self) -> Result<(), anyhow::Error> {
        if matches!(self.state, ServiceState::Running(_)) {
            self.stop();
        }

        if let ServiceState::Stopped(worker_handle) = self.state {
            worker_handle.await.context("Failed to join worker")?;
            Ok(())
        } else {
            warn!("LedgerTruncator was not running, nothing to stop");
            Ok(())
        }
    }
}
