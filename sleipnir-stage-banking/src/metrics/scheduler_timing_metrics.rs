// From: core/src/banking_stage/transaction_scheduler/scheduler_controller.rs :622

use solana_metrics::datapoint_info;
use solana_sdk::timing::AtomicInterval;

#[derive(Default)]
pub(crate) struct SchedulerTimingMetrics {
    interval: AtomicInterval,
    /// Time spent making processing decisions.
    decision_time_us: u64,
    /// Time spent receiving packets.
    receive_time_us: u64,
    /// Time spent buffering packets.
    buffer_time_us: u64,
    /// Time spent filtering transactions during scheduling.
    schedule_filter_time_us: u64,
    /// Time spent scheduling transactions.
    schedule_time_us: u64,
    /// Time spent clearing transactions from the container.
    clear_time_us: u64,
    /// Time spent cleaning expired or processed transactions from the container.
    clean_time_us: u64,
    /// Time spent receiving completed transactions.
    receive_completed_time_us: u64,
}

impl SchedulerTimingMetrics {
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
            "banking_stage_scheduler_timing",
            ("decision_time_us", self.decision_time_us, i64),
            ("receive_time_us", self.receive_time_us, i64),
            ("buffer_time_us", self.buffer_time_us, i64),
            ("schedule_filter_time_us", self.schedule_filter_time_us, i64),
            ("schedule_time_us", self.schedule_time_us, i64),
            ("clear_time_us", self.clear_time_us, i64),
            ("clean_time_us", self.clean_time_us, i64),
            (
                "receive_completed_time_us",
                self.receive_completed_time_us,
                i64
            )
        );
    }

    fn reset(&mut self) {
        self.decision_time_us = 0;
        self.receive_time_us = 0;
        self.buffer_time_us = 0;
        self.schedule_filter_time_us = 0;
        self.schedule_time_us = 0;
        self.clear_time_us = 0;
        self.clean_time_us = 0;
        self.receive_completed_time_us = 0;
    }
}
