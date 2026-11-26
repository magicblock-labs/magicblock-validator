//! Event-driven table_mania metrics registered in the default Prometheus registry.

use nucleus::metrics::{IntCounter, Opts, counter};
use std::sync::OnceLock;

static METRICS: OnceLock<Metrics> = OnceLock::new();

struct Metrics {
    fetches: IntCounter,
    closure_checks: IntCounter,
}

fn metrics() -> &'static Metrics {
    METRICS.get_or_init(|| Metrics {
        fetches: counter(
            Opts::new(
                "lookup_table_readiness_fetch_attempts_total",
                "Remote lookup-table readiness fetch attempts",
            ),
            0,
        ),
        closure_checks: counter(
            Opts::new(
                "lookup_table_closure_checks_total",
                "Remote lookup-table closure checks",
            ),
            0,
        ),
    })
}

pub(crate) fn readiness_fetch() {
    metrics().fetches.inc();
}
pub(crate) fn closure_check() {
    metrics().closure_checks.inc();
}
