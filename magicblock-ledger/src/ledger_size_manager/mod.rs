#![allow(unused)]

pub mod config;
pub mod errors;
mod watermarks;

use config::{ExistingLedgerState, LedgerSizeManagerConfig, ResizePercentage};
use errors::LedgerSizeManagerResult;
use log::*;
use std::{collections::VecDeque, sync::Arc, time::Duration};
use watermarks::{Watermark, Watermarks};

use magicblock_metrics::metrics;
use solana_sdk::clock::Slot;
use thiserror::Error;
use tokio::{
    task::{JoinError, JoinHandle},
    time::interval,
};
use tokio_util::sync::CancellationToken;

use crate::Ledger;

enum ServiceState {
    Created {
        size_check_interval: Duration,
        resize_percentage: ResizePercentage,
    },
    Running {
        cancellation_token: CancellationToken,
        worker_handle: JoinHandle<()>,
    },
    Stopped {
        worker_handle: JoinHandle<()>,
    },
}

pub struct LedgerSizeManager {
    ledger: Option<Arc<Ledger>>,
    watermarks: Watermarks,
    service_state: ServiceState,
}

impl LedgerSizeManager {
    pub(crate) fn new(
        ledger: Option<Arc<Ledger>>,
        ledger_state: Option<ExistingLedgerState>,
        config: LedgerSizeManagerConfig,
    ) -> Self {
        let watermarks = Watermarks::new(
            &config.resize_percentage,
            config.max_size,
            ledger_state,
        );
        LedgerSizeManager {
            ledger,
            watermarks,
            service_state: ServiceState::Created {
                size_check_interval: Duration::from_millis(
                    config.size_check_interval_ms,
                ),
                resize_percentage: config.resize_percentage,
            },
        }
    }

    fn ensure_initial_max_ledger_size(&self) {
        // TODO: @@@ wait for fix/ledger/delete-using-compaction-filter to be merged which
        // includes a _fat_ ledger truncate
        // We will run that first to get below the ledger max size _before_ switching to
        // the watermark strategy.
    }

    pub fn try_start(self) -> LedgerSizeManagerResult<Self> {
        /*
        let Some(ledger) = self.ledger.take() else {
            return Err(LedgerSizeManagerError::LedgerNotProvided);
        };
        if let ServiceState::Created {
            size_check_interval,
            resize_percentage,
        } = self.service_state
        {
            let cancellation_token = CancellationToken::new();
            let worker_handle = {
                ledger.initialize_lowest_cleanup_slot()?;
                let mut interval = interval(size_check_interval);
                let cancellation_token = cancellation_token.clone();
                tokio::spawn(async move {
                    loop {
                        tokio::select! {
                            _ = cancellation_token.cancelled() => {
                                return;
                            }
                            _ = interval.tick() => {
                                    let Ok(ledger_size) = ledger.storage_size() else {
                                                eprintln!(
                                            "Failed to get ledger size, cannot start LedgerSizeManager"
                                        );
                                        continue;
                                    };
                                    metrics::set_ledger_size(ledger_size);
                                    if ledger_size > self.watermarks.max_ledger_size {
                                        self.ensure_initial_max_ledger_size();
                                        continue;
                                    }
                                    if self.watermarks.is_empty() {
                                    self.watermarks = Watermarks::new(
                                        &resize_percentage,
                                        self.watermarks.max_ledger_size,
                                        Some(ExistingLedgerState {
                                            size: ledger_size,
                                            slot: ledger.last_slot(),
                                            mod_id: ledger.last_mod_id(),
                                        }),
                                    );
                                    }



                                    if let Some(mark) = self.get_truncation_mark(
                                        ledger_size,
                                        ledger.last_slot(),
                                        ledger.last_mod_id(),
                                    ) {
                                        self.truncate(&ledger, mark);
                                    }
                                }
                        }
                    }
                })
            };
            self.service_state = ServiceState::Running {
                cancellation_token,
                worker_handle,
            };
            todo!()
        } else {
            warn!("LedgerSizeManager already running, no need to start.");
            todo!()
        }
        */
        todo!()
    }

    fn truncate(&self, ledger: &Arc<Ledger>, mark: Watermark) {
        // This is where the truncation logic would go.
        // For now, we just print the truncation mark.
        debug!(
            "Truncating ledger at slot {}, mod_id {}, size {}",
            mark.slot, mark.mod_id, mark.size
        );
    }
}
