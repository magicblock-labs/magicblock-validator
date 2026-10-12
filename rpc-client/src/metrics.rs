//! Event-driven rpc_client metrics registered in the default Prometheus registry.

use nucleus::metrics::{IntCounter, Opts, counter};
use std::sync::OnceLock;

static METRICS: OnceLock<Metrics> = OnceLock::new();

struct Metrics {
    ws_subscribe: IntCounter,
    ws_notification: IntCounter,
    ws_fallback: IntCounter,
    status_batch: IntCounter,
    status_signatures: IntCounter,
}

fn metrics() -> &'static Metrics {
    METRICS.get_or_init(|| Metrics {
        ws_subscribe: counter(
            Opts::new(
                "rpc_client_signature_ws_subscription_attempts_total",
                "Signature WebSocket subscription attempts",
            ),
            0,
        ),
        ws_notification: counter(
            Opts::new(
                "rpc_client_signature_ws_notifications_total",
                "Signature WebSocket notifications",
            ),
            0,
        ),
        ws_fallback: counter(
            Opts::new(
                "rpc_client_signature_http_fallbacks_total",
                "Signature confirmation fallbacks to HTTP",
            ),
            0,
        ),
        status_batch: counter(
            Opts::new(
                "rpc_client_signature_status_batches_total",
                "Signature status HTTP batches",
            ),
            0,
        ),
        status_signatures: counter(
            Opts::new(
                "rpc_client_signature_status_entries_total",
                "Signatures queried in HTTP status batches",
            ),
            0,
        ),
    })
}

pub(crate) fn subscribe() {
    metrics().ws_subscribe.inc();
}
pub(crate) fn notification() {
    metrics().ws_notification.inc();
}
pub(crate) fn fallback() {
    metrics().ws_fallback.inc();
}
pub(crate) fn status_batch(count: usize) {
    let m = metrics();
    m.status_batch.inc();
    m.status_signatures.inc_by(count as u64);
}
