use itertools::MinMaxResult;
use solana_metrics::datapoint_info;
use solana_sdk::timing::AtomicInterval;

// From: core/src/banking_stage/transaction_scheduler/scheduler_controller.rs :440
#[derive(Default)]
pub(crate) struct SchedulerCountMetrics {
    interval: AtomicInterval,

    /// Number of packets received.
    pub(crate) num_received: usize,
    /// Number of packets buffered.
    pub(crate) num_buffered: usize,

    /// Number of transactions scheduled.
    pub(crate) num_scheduled: usize,
    /// Number of transactions that were unschedulable.
    pub(crate) num_unschedulable: usize,
    /// Number of transactions that were filtered out during scheduling.
    pub(crate) num_schedule_filtered_out: usize,
    /// Number of completed transactions received from workers.
    pub(crate) num_finished: usize,
    /// Number of transactions that were retryable.
    pub(crate) num_retryable: usize,

    /// Number of transactions that were immediately dropped on receive.
    pub(crate) num_dropped_on_receive: usize,
    /// Number of transactions that were dropped due to sanitization failure.
    pub(crate) num_dropped_on_sanitization: usize,
    /// Number of transactions that were dropped due to failed lock validation.
    pub(crate) num_dropped_on_validate_locks: usize,
    /// Number of transactions that were dropped due to failed transaction
    /// checks during receive.
    pub(crate) num_dropped_on_receive_transaction_checks: usize,
    /// Number of transactions that were dropped due to clearing.
    pub(crate) num_dropped_on_clear: usize,
    /// Number of transactions that were dropped due to age and status checks.
    pub(crate) num_dropped_on_age_and_status: usize,
    /// Number of transactions that were dropped due to exceeded capacity.
    pub(crate) num_dropped_on_capacity: usize,
    /// Min prioritization fees in the transaction container
    pub(crate) min_prioritization_fees: u64,
    /// Max prioritization fees in the transaction container
    pub(crate) max_prioritization_fees: u64,
}

impl SchedulerCountMetrics {
    fn maybe_report_and_reset(&mut self, should_report: bool) {
        const REPORT_INTERVAL_MS: u64 = 1000;
        if self.interval.should_update(REPORT_INTERVAL_MS) {
            if should_report {
                self.report();
            }
            self.reset();
        }
    }

    fn report(&self) {
        datapoint_info!(
            "banking_stage_scheduler_counts",
            ("num_received", self.num_received, i64),
            ("num_buffered", self.num_buffered, i64),
            ("num_scheduled", self.num_scheduled, i64),
            ("num_unschedulable", self.num_unschedulable, i64),
            (
                "num_schedule_filtered_out",
                self.num_schedule_filtered_out,
                i64
            ),
            ("num_finished", self.num_finished, i64),
            ("num_retryable", self.num_retryable, i64),
            ("num_dropped_on_receive", self.num_dropped_on_receive, i64),
            (
                "num_dropped_on_sanitization",
                self.num_dropped_on_sanitization,
                i64
            ),
            (
                "num_dropped_on_validate_locks",
                self.num_dropped_on_validate_locks,
                i64
            ),
            (
                "num_dropped_on_receive_transaction_checks",
                self.num_dropped_on_receive_transaction_checks,
                i64
            ),
            ("num_dropped_on_clear", self.num_dropped_on_clear, i64),
            (
                "num_dropped_on_age_and_status",
                self.num_dropped_on_age_and_status,
                i64
            ),
            ("num_dropped_on_capacity", self.num_dropped_on_capacity, i64),
            ("min_priority", self.get_min_priority(), i64),
            ("max_priority", self.get_max_priority(), i64)
        );
    }

    fn has_data(&self) -> bool {
        self.num_received != 0
            || self.num_buffered != 0
            || self.num_scheduled != 0
            || self.num_unschedulable != 0
            || self.num_schedule_filtered_out != 0
            || self.num_finished != 0
            || self.num_retryable != 0
            || self.num_dropped_on_receive != 0
            || self.num_dropped_on_sanitization != 0
            || self.num_dropped_on_validate_locks != 0
            || self.num_dropped_on_receive_transaction_checks != 0
            || self.num_dropped_on_clear != 0
            || self.num_dropped_on_age_and_status != 0
            || self.num_dropped_on_capacity != 0
    }

    fn reset(&mut self) {
        self.num_received = 0;
        self.num_buffered = 0;
        self.num_scheduled = 0;
        self.num_unschedulable = 0;
        self.num_schedule_filtered_out = 0;
        self.num_finished = 0;
        self.num_retryable = 0;
        self.num_dropped_on_receive = 0;
        self.num_dropped_on_sanitization = 0;
        self.num_dropped_on_validate_locks = 0;
        self.num_dropped_on_receive_transaction_checks = 0;
        self.num_dropped_on_clear = 0;
        self.num_dropped_on_age_and_status = 0;
        self.num_dropped_on_capacity = 0;
        self.min_prioritization_fees = u64::MAX;
        self.max_prioritization_fees = 0;
    }

    pub fn update_priority_stats(&mut self, min_max_fees: MinMaxResult<u64>) {
        // update min/max priority
        match min_max_fees {
            itertools::MinMaxResult::NoElements => {
                // do nothing
            }
            itertools::MinMaxResult::OneElement(e) => {
                self.min_prioritization_fees = e;
                self.max_prioritization_fees = e;
            }
            itertools::MinMaxResult::MinMax(min, max) => {
                self.min_prioritization_fees = min;
                self.max_prioritization_fees = max;
            }
        }
    }

    pub fn get_min_priority(&self) -> u64 {
        // to avoid getting u64::max recorded by metrics / in case of edge cases
        if self.min_prioritization_fees != u64::MAX {
            self.min_prioritization_fees
        } else {
            0
        }
    }

    pub fn get_max_priority(&self) -> u64 {
        self.max_prioritization_fees
    }
}
