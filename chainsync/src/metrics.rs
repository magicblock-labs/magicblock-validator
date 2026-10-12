//! Process-wide snapshot and transport metrics.
use crate::http::{Error as HttpError, Outcome};
use nucleus::metrics::{
    IntCounterVec, MetricOperation, MetricSpec, OperationCounters, OperationTimer, counter_vec,
    with_metrics,
};
use std::sync::OnceLock;
static METRICS: OnceLock<Metrics> = OnceLock::new();
#[derive(Clone, Copy)]
pub(crate) enum Op {
    HttpFetch,
    HttpAttempt,
}
impl MetricOperation for Op {
    fn label(self) -> &'static str {
        match self {
            Self::HttpFetch => "http_fetch",
            Self::HttpAttempt => "http_attempt",
        }
    }
}
/// Subscription source and transport-failure label shared by the workers.
#[derive(Clone, Copy)]
pub(crate) enum Transport {
    WebSocket,
    Grpc,
}

impl Transport {
    fn label(self) -> &'static str {
        match self {
            Self::WebSocket => "ws",
            Self::Grpc => "grpc",
        }
    }
}

struct Metrics {
    durations: OperationCounters,
    http: IntCounterVec,
    transport: IntCounterVec,
}
pub(crate) fn init() {
    METRICS.get_or_init(|| Metrics {
        durations: OperationCounters::new(spec(
            "chainsync_duration_micros",
            "ChainSync operation duration in microseconds",
        )),
        http: counter_vec(
            spec("chainsync_http_attempts", "HTTP attempt results"),
            &["outcome"],
        ),
        transport: counter_vec(
            spec(
                "chainsync_transport_failures",
                "WS attempt and gRPC outer-session failures",
            ),
            &["transport"],
        ),
    });
}
fn spec(name: &'static str, help: &'static str) -> MetricSpec {
    MetricSpec { name, help }
}

/// Records elapsed microseconds on drop; a no-op before initialization.
pub(crate) fn time(op: Op) -> OperationTimer<'static> {
    op.time(METRICS.get().map(|metrics| &metrics.durations))
}

/// Counts one failed WS attempt or gRPC outer session by transport.
/// Yellowstone's internal reconnects do not enter this boundary.
pub(crate) fn transport(transport: Transport) {
    with_metrics(&METRICS, |metrics| {
        metrics.transport.with_label_values(&[transport.label()]).inc();
    });
}

/// Counts a completed HTTP attempt using the retry policy's error categories.
pub(crate) fn http_attempt<T>(result: &Result<T, HttpError>) {
    with_metrics(&METRICS, |metrics| {
        let outcome = result.as_ref().err().map_or(Outcome::Success, HttpError::outcome);
        metrics.http.with_label_values(&[outcome.label()]).inc();
    });
}
