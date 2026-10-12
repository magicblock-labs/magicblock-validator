//! Event-driven validator metrics registered in the default Prometheus registry.

use nucleus::metrics::{Histogram, HistogramOpts, HistogramTimer, histogram};
use std::sync::OnceLock;

static METRICS: OnceLock<Metrics> = OnceLock::new();

struct Metrics {
    nonce_wait: Histogram,
}

fn metrics() -> &'static Metrics {
    METRICS.get_or_init(|| Metrics {
        nonce_wait: histogram(
            HistogramOpts::new(
                "validator_commit_nonce_wait_duration_seconds",
                "Synchronous commit-nonce wait duration in seconds",
            )
            .buckets(vec![0.001, 0.01, 0.1, 1.0, 5.0, 10.0, 30.0, 60.0]),
        ),
    })
}

pub(crate) fn time_nonce_wait() -> HistogramTimer {
    metrics().nonce_wait.start_timer()
}
