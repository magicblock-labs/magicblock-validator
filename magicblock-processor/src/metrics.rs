#![allow(dead_code)]
use std::collections::HashMap;

use solana_metrics::datapoint_info;
use solana_sdk::{clock::Slot, saturating_add_assign};
use solana_timings::{ExecuteTimingType, ExecuteTimings};
#[derive(Debug, Default)]
pub struct ThreadExecuteTimings {
    pub total_thread_us: u64,
    pub total_transactions_executed: u64,
    pub execute_timings: ExecuteTimings,
}

impl ThreadExecuteTimings {
    pub fn accumulate(&mut self, other: &ThreadExecuteTimings) {
        self.execute_timings.accumulate(&other.execute_timings);
        saturating_add_assign!(self.total_thread_us, other.total_thread_us);
        saturating_add_assign!(
            self.total_transactions_executed,
            other.total_transactions_executed
        );
    }
}

// NOTE: copied from ledger/src/blockstore_processor.rs :218
#[derive(Default)]
pub struct ExecuteBatchesInternalMetrics {
    pub(super) execution_timings_per_thread:
        HashMap<usize, ThreadExecuteTimings>,
    pub(super) total_batches_len: u64,
    pub(super) execute_batches_us: u64,
}

impl ExecuteBatchesInternalMetrics {
    pub fn new_with_timings_from_all_threads(
        execute_timings: ExecuteTimings,
    ) -> Self {
        const DUMMY_THREAD_INDEX: usize = 999;
        let mut new = Self::default();
        new.execution_timings_per_thread.insert(
            DUMMY_THREAD_INDEX,
            ThreadExecuteTimings {
                execute_timings,
                ..ThreadExecuteTimings::default()
            },
        );
        new
    }
}

/// Measures times related to transaction execution in a slot.
#[derive(Debug, Default)]
pub struct BatchExecutionTiming {
    /// Time used by transaction execution.  Accumulated across multiple threads that are running
    /// `execute_batch()`.
    pub totals: ExecuteTimings,

    /// Wall clock time used by the transaction execution part of pipeline.
    /// [`ConfirmationTiming::replay_elapsed`] includes this time.  In microseconds.
    pub wall_clock_us: u64,

    /// Time used to execute transactions, via `execute_batch()`, in the thread that consumed the
    /// most time.
    pub slowest_thread: ThreadExecuteTimings,
}

impl BatchExecutionTiming {
    pub fn accumulate(&mut self, new_batch: ExecuteBatchesInternalMetrics) {
        let Self {
            totals,
            wall_clock_us,
            slowest_thread,
        } = self;

        saturating_add_assign!(*wall_clock_us, new_batch.execute_batches_us);

        use ExecuteTimingType::{NumExecuteBatches, TotalBatchesLen};
        totals.saturating_add_in_place(
            TotalBatchesLen,
            new_batch.total_batches_len,
        );
        totals.saturating_add_in_place(NumExecuteBatches, 1);

        for thread_times in new_batch.execution_timings_per_thread.values() {
            totals.accumulate(&thread_times.execute_timings);
        }

        let slowest = new_batch
            .execution_timings_per_thread
            .values()
            .max_by_key(|thread_times| thread_times.total_thread_us);

        if let Some(slowest) = slowest {
            slowest_thread.accumulate(slowest);
            slowest_thread
                .execute_timings
                .saturating_add_in_place(NumExecuteBatches, 1);
        };
    }
}
