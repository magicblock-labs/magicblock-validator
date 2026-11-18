//! Event-driven aperture metrics registered in the default Prometheus registry.

use nucleus::metrics::{
    HistogramOpts, HistogramTimer, HistogramVec, IntCounter, IntCounterVec, Opts, counter,
    counter_vec, histogram_vec,
};
use std::sync::OnceLock;

static METRICS: OnceLock<Metrics> = OnceLock::new();

struct Metrics {
    requests: IntCounterVec,
    duration: HistogramVec,
    skip_preflight: IntCounter,
}

fn metrics() -> &'static Metrics {
    METRICS.get_or_init(|| Metrics {
        requests: counter_vec(
            Opts::new("aperture_rpc_requests_total", "RPC requests by method"),
            &["method"],
        ),
        duration: histogram_vec(
            HistogramOpts::new(
                "aperture_rpc_request_duration_seconds",
                "HTTP RPC request duration in seconds",
            )
            .buckets(vec![0.001, 0.01, 0.1, 1.0, 5.0, 10.0, 30.0, 60.0]),
            &["method"],
        ),
        skip_preflight: counter(
            Opts::new(
                "aperture_transactions_skip_preflight_total",
                "Transactions submitted with skipPreflight",
            ),
            0,
        ),
    })
}

pub(crate) fn request(method: &str) {
    metrics().requests.with_label_values(&[method]).inc();
}
pub(crate) fn time_request(method: &str) -> HistogramTimer {
    metrics().duration.with_label_values(&[method]).start_timer()
}
pub(crate) fn skip_preflight() {
    metrics().skip_preflight.inc();
}
