//! Event-driven committor metrics registered in the default Prometheus registry.

use crate::{
    intent_executor::{ExecutionOutput, error::IntentExecutorError},
    tasks::BaseTaskImpl,
};
use nucleus::metrics::{
    Histogram, HistogramOpts, HistogramTimer, HistogramVec, IntCounter, IntCounterVec, IntGauge,
    Opts, counter, counter_vec, gauge, gauge_value, histogram, histogram_vec,
};
use std::sync::OnceLock;

const DURATION_BUCKETS: [f64; 8] = [0.001, 0.01, 0.1, 1.0, 5.0, 10.0, 30.0, 60.0];

static METRICS: OnceLock<Metrics> = OnceLock::new();

struct Metrics {
    scheduled: IntCounter,
    backlog: IntGauge,
    inflight: IntGauge,
    poisoned: IntGauge,
    voided: IntCounter,
    failures: IntCounterVec,
    execution: HistogramVec,
    task_preparation: HistogramVec,
    alt_preparation: Histogram,
    alt_count: Histogram,
    account_fetches: IntCounter,
    retiring: IntGauge,
    history: Histogram,
}

fn metrics() -> &'static Metrics {
    METRICS.get_or_init(|| Metrics {
        scheduled: counter(
            Opts::new(
                "committor_scheduled_intents_total",
                "Live intents submitted for execution",
            ),
            0,
        ),
        backlog: gauge(
            Opts::new(
                "committor_backlog_intents",
                "Intents queued in the account-backed backlog",
            ),
            0,
        ),
        inflight: gauge(
            Opts::new(
                "committor_inflight_intents",
                "Running intent tasks including retries",
            ),
            0,
        ),
        poisoned: gauge(
            Opts::new(
                "committor_poisoned_keys",
                "Keys poisoned in the intent scheduler",
            ),
            0,
        ),
        voided: counter(
            Opts::new(
                "committor_voided_intents_total",
                "Intents voided by a poisoning cascade",
            ),
            0,
        ),
        failures: counter_vec(
            Opts::new("committor_failed_intents_total", "Terminal intent failures"),
            &["error"],
        ),
        execution: histogram_vec(
            HistogramOpts::new(
                "committor_intent_execution_duration_seconds",
                "Intent execution duration including retries in seconds",
            )
            .buckets(DURATION_BUCKETS.to_vec()),
            &["outcome"],
        ),
        task_preparation: histogram_vec(
            HistogramOpts::new(
                "committor_task_preparation_duration_seconds",
                "Task preparation duration in seconds",
            )
            .buckets(DURATION_BUCKETS.to_vec()),
            &["task"],
        ),
        alt_preparation: histogram(
            HistogramOpts::new(
                "committor_lookup_table_preparation_duration_seconds",
                "Lookup-table preparation duration in seconds",
            )
            .buckets(DURATION_BUCKETS.to_vec()),
        ),
        alt_count: histogram(
            HistogramOpts::new(
                "committor_transaction_lookup_tables",
                "Lookup tables per prepared transaction when present",
            )
            .buckets(vec![0.0, 1.0, 2.0, 3.0, 4.0, 5.0, 10.0]),
        ),
        account_fetches: counter(
            Opts::new(
                "committor_account_fetch_attempts_total",
                "Task account RPC fetch attempts",
            ),
            0,
        ),
        retiring: gauge(
            Opts::new(
                "committor_retiring_nonce_locks",
                "Evicted nonce locks still held by active operations",
            ),
            0,
        ),
        history: histogram(
            HistogramOpts::new(
                "committor_signature_history_query_duration_seconds",
                "Recovery signature-history query duration in seconds",
            )
            .buckets(DURATION_BUCKETS.to_vec()),
        ),
    })
}

pub(crate) fn scheduled(count: usize) {
    metrics().scheduled.inc_by(count as u64);
}
pub(crate) fn backlog_added(count: usize) {
    metrics().backlog.add(gauge_value(count));
}
pub(crate) fn backlog_removed(count: usize) {
    metrics().backlog.sub(gauge_value(count));
}
pub(crate) fn poisoned_added(count: usize) {
    metrics().poisoned.add(gauge_value(count));
}
pub(crate) fn poisoned_removed(count: usize) {
    metrics().poisoned.sub(gauge_value(count));
}
pub(crate) fn voided(count: usize) {
    metrics().voided.inc_by(count as u64);
}
pub(crate) fn execution(seconds: f64, result: &Result<ExecutionOutput, IntentExecutorError>) {
    let outcome = match result {
        Ok(output) => output.metric_label(),
        Err(_) => "failed",
    };
    metrics().execution.with_label_values(&[outcome]).observe(seconds);
    if let Err(error) = result {
        metrics().failures.with_label_values(&[error.metric_label()]).inc();
    }
}
pub(crate) fn time_task(task: &BaseTaskImpl) -> HistogramTimer {
    metrics()
        .task_preparation
        .with_label_values(&[task.metric_label()])
        .start_timer()
}
pub(crate) fn time_alt() -> HistogramTimer {
    metrics().alt_preparation.start_timer()
}
pub(crate) fn lookup_tables(count: usize) {
    metrics().alt_count.observe(count as f64);
}
pub(crate) fn account_fetch() {
    metrics().account_fetches.inc();
}
pub(crate) fn retiring(before: usize, after: usize) {
    metrics().retiring.add(gauge_value(after) - gauge_value(before));
}
pub(crate) fn time_history() -> HistogramTimer {
    metrics().history.start_timer()
}

/// Tracks a running intent through completion, panic, or cancellation.
pub(crate) struct Inflight;
impl Inflight {
    pub(crate) fn new() -> Self {
        metrics().inflight.inc();
        Self
    }
}
impl Drop for Inflight {
    fn drop(&mut self) {
        metrics().inflight.dec();
    }
}
