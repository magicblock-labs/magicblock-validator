#![allow(unused)]

pub mod config;
pub mod errors;
pub mod traits;
mod truncator;
mod watermarks;

use config::{ExistingLedgerState, LedgerSizeManagerConfig, ResizePercentage};
use errors::{LedgerSizeManagerError, LedgerSizeManagerResult};
use log::*;
use std::{collections::VecDeque, sync::Arc, time::Duration};
use traits::ManagableLedger;
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
        max_ledger_size: u64,
        existing_ledger_state: Option<ExistingLedgerState>,
    },
    Running {
        cancellation_token: CancellationToken,
        worker_handle: JoinHandle<()>,
    },
    Stopped {
        worker_handle: JoinHandle<()>,
    },
}

pub struct LedgerSizeManager<T: ManagableLedger> {
    ledger: Arc<T>,
    service_state: ServiceState,
}

impl<T: ManagableLedger> LedgerSizeManager<T> {
    pub(crate) fn new(
        ledger: Arc<T>,
        ledger_state: Option<ExistingLedgerState>,
        config: LedgerSizeManagerConfig,
    ) -> Self {
        LedgerSizeManager {
            ledger,
            service_state: ServiceState::Created {
                size_check_interval: Duration::from_millis(
                    config.size_check_interval_ms,
                ),
                resize_percentage: config.resize_percentage,
                max_ledger_size: config.max_size,
                existing_ledger_state: ledger_state,
            },
        }
    }

    pub fn try_start(self) -> LedgerSizeManagerResult<Self> {
        if let ServiceState::Created {
            size_check_interval,
            resize_percentage,
            max_ledger_size,
            mut existing_ledger_state,
        } = self.service_state
        {
            let cancellation_token = CancellationToken::new();
            let worker_handle = {
                let ledger = self.ledger.clone();
                ledger.initialize_lowest_cleanup_slot()?;
                let mut interval = interval(size_check_interval);
                let mut watermarks = None::<Watermarks>;

                let mut cancellation_token = cancellation_token.clone();
                tokio::spawn(async move {
                    loop {
                        tokio::select! {
                            _ = cancellation_token.cancelled() => {
                                return;
                            }
                            _ = interval.tick() => {
                                    let Ok(ledger_size) = ledger.storage_size() else {
                                        error!(
                                            "Failed to get ledger size, cannot manage its size"
                                        );
                                        continue;
                                    };

                                    metrics::set_ledger_size(ledger_size);

                                    // If we restarted with an existing ledger we need to make sure that the
                                    // ledger size is not far above the max size before we can
                                    // start using the watermark strategy.
                                    if watermarks.is_none() &&
                                        existing_ledger_state.is_some() &&
                                        ledger_size > max_ledger_size {
                                        warn!(
                                            "Ledger size {} is above the max size {}, \
                                            waiting for truncation to start using watermarks.",
                                            ledger_size, max_ledger_size
                                        );
                                        Self::ensure_initial_max_ledger_size_below(
                                            &ledger,
                                            max_ledger_size);
                                        continue;
                                    }

                                    // We either started new or trimmed the existing ledger to
                                    // below the max size and now will keep it so using watermarks.
                                    let wms = watermarks
                                        .get_or_insert_with(|| Watermarks::new(
                                            &resize_percentage,
                                            max_ledger_size,
                                            existing_ledger_state.take(),
                                        ));

                                    if let Some(mark) = wms.get_truncation_mark(
                                        ledger_size,
                                        ledger.last_slot(),
                                        ledger.last_mod_id(),
                                    ) {
                                        Self::truncate_ledger(&ledger, mark);
                                    }
                                }
                        }
                    }
                })
            };
            Ok(Self {
                ledger: self.ledger,
                service_state: ServiceState::Running {
                    cancellation_token,
                    worker_handle,
                },
            })
        } else {
            warn!("LedgerSizeManager already running, no need to start.");
            Ok(self)
        }
    }

    pub fn stop(self) -> Self {
        match self.service_state {
            ServiceState::Running {
                cancellation_token,
                worker_handle,
            } => {
                cancellation_token.cancel();
                Self {
                    ledger: self.ledger,
                    service_state: ServiceState::Stopped { worker_handle },
                }
            }
            _ => {
                warn!("LedgerSizeManager is not running, cannot stop.");
                self
            }
        }
    }

    fn ensure_initial_max_ledger_size_below(ledger: &Arc<T>, max_size: u64) {
        // TODO: @@@ wait for fix/ledger/delete-using-compaction-filter to be merged which
        // includes a _fat_ ledger truncate
        // We will run that first to get below the ledger max size _before_ switching to
        // the watermark strategy.
    }

    fn truncate_ledger(ledger: &Arc<T>, mark: Watermark) {
        // This is where the truncation logic would go.
        // For now, we just print the truncation mark.
        debug!(
            "Truncating ledger at slot {}, mod_id {}, size {}",
            mark.slot, mark.mod_id, mark.size
        );
    }
}
