// NOTE: from core/src/sample_performance_service.rs
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread::{self, sleep, Builder, JoinHandle},
    time::{Duration, Instant},
};

use log::*;
use magicblock_bank::bank::Bank;
use magicblock_ledger::{Ledger, PerfSample};

use crate::stats_snapshot::StatsSnapshot;

const SAMPLE_INTERVAL: Duration = Duration::from_secs(60);
const SLEEP_INTERVAL: Duration = Duration::from_millis(500);

pub struct SamplePerformanceService {
    thread_hdl: JoinHandle<()>,
}

impl SamplePerformanceService {
    pub fn new(
        bank: &Arc<Bank>,
        ledger: &Arc<Ledger>,
        exit: Arc<AtomicBool>,
    ) -> Self {
        let bank = bank.clone();
        let ledger = ledger.clone();

        let thread_hdl = Builder::new()
            .name("solSamplePerf".to_string())
            .spawn(move || {
                info!("SamplePerformanceService has started");
                Self::run(&bank, &ledger, exit);
                info!("SamplePerformanceService has stopped");
            })
            .unwrap();

        Self { thread_hdl }
    }

    fn run(bank: &Bank, ledger: &Ledger, exit: Arc<AtomicBool>) {
        let mut snapshot = StatsSnapshot::from_bank(bank);
        let mut last_sample_time = Instant::now();

        // NOTE: we'll have a different mechanism via tokio cancellation token
        // to exit these long running tasks
        while !exit.load(Ordering::Relaxed) {
            let elapsed = last_sample_time.elapsed();
            if elapsed >= SAMPLE_INTERVAL {
                last_sample_time = Instant::now();
                let new_snapshot = StatsSnapshot::from_bank(bank);

                let (num_transactions, num_non_vote_transactions, num_slots) =
                    new_snapshot.diff_since(&snapshot);

                // Store the new snapshot to compare against in the next iteration of the loop.
                snapshot = new_snapshot;

                let perf_sample = PerfSample {
                    // Note: since num_slots is computed from the highest slot and not the bank
                    // slot, this value should not be used in conjunction with num_transactions or
                    // num_non_vote_transactions to draw any conclusions about number of
                    // transactions per slot.
                    num_slots,
                    num_transactions,
                    num_non_vote_transactions,
                    sample_period_secs: elapsed.as_secs() as u16,
                };

                let highest_slot = snapshot.highest_slot;
                if let Err(e) =
                    ledger.write_perf_sample(highest_slot, &perf_sample)
                {
                    error!(
                        "write_perf_sample failed: slot {:?} {:?}",
                        highest_slot, e
                    );
                }
            }
            sleep(SLEEP_INTERVAL);
        }
    }

    pub fn join(self) -> thread::Result<()> {
        self.thread_hdl.join()
    }
}
