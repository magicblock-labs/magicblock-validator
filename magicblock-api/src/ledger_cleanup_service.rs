use std::{mem::take, sync::Arc, time::Duration};

use log::{error, warn};
use magicblock_ledger::Ledger;
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
    size_thresholds_bytes: u64,
    state: ServiceState,
}

impl LedgerCleanupService {
    // TODO: probably move to config
    const CLEANUP_INTERVAL: Duration = Duration::from_secs(10 * 60);

    pub fn new(ledger: Arc<Ledger>, size_thresholds_bytes: u64) -> Self {
        Self {
            ledger,
            size_thresholds_bytes,
            state: ServiceState::Created,
        }
    }

    pub fn start(&mut self) {
        if let ServiceState::Created = self.state {
            let cancellation_token = CancellationToken::new();
            let worker_handle = tokio::spawn(Self::worker(
                self.ledger.clone(),
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
                        Ok(size)  => {
                            if size >= size_thresholds_bytes {
                                Self::cleanup(ledger.clone()).await;
                            }
                        }
                        Err(err) => error!("Failed to get Ledger storage size: {}", err)
                    }
                }
            }
        }
    }

    async fn cleanup(ledger: Arc<Ledger>) {}
}
