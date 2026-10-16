//! Process-wide ChainSync metrics with fixed outcome and transport labels.

use std::sync::OnceLock;

use nucleus::metrics::{
    IntCounter, IntCounterVec, IntGaugeVec, MetricOperation, MetricSpec, OperationCounters,
    OperationTimer, counter, counter_vec, gauge_vec, with_metrics,
};

use crate::http::{Error as HttpError, Outcome};

/// Shared collectors registered once for all ChainSync instances.
static METRICS: OnceLock<Metrics> = OnceLock::new();

/// Account synchronization operations measured by duration.
#[derive(Clone, Copy)]
pub(crate) enum Op {
    /// All acquisition waves, dependencies, leases, and materialization.
    ChainSync,
    /// A complete HTTP fetch, including retries and cooldown waits.
    HttpFetch,
    /// Applying an update from an active subscription, including time waiting for its Engine lease.
    Apply,
    /// Non-confined delegation materialization, including assessment and rescue.
    Delegate,
    /// Handling one undelegation event, including any projected ATA lookup.
    Undelegate,
    /// One provider request, including response decoding.
    HttpAttempt,
}

impl MetricOperation for Op {
    fn label(self) -> &'static str {
        match self {
            Self::ChainSync => "chainsync",
            Self::HttpFetch => "http_fetch",
            Self::Apply => "apply",
            Self::Delegate => "delegate",
            Self::Undelegate => "undelegate",
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

/// Process-wide collectors; labels are selected at each recording boundary.
struct Metrics {
    /// Elapsed microseconds grouped by operation.
    durations: OperationCounters,
    /// Active account subscriptions grouped by WebSocket-only, gRPC-only, or both transports.
    coverage: IntGaugeVec,
    /// Account subscriptions that lost their last active transport unexpectedly.
    losses: IntCounterVec,
    /// Failed activations that enter rescue, regardless of its outcome.
    activation_failures: IntCounter,
    /// Rescue scheduling results and superseded-image decisions.
    rescues: IntCounterVec,
    /// Completed HTTP attempts grouped by result category.
    http: IntCounterVec,
    /// WS attempt and gRPC outer-session failures grouped by transport.
    transport: IntCounterVec,
}

/// Registers namespaced collectors once; subsequent calls reuse them.
pub(crate) fn init() {
    METRICS.get_or_init(|| Metrics {
        durations: OperationCounters::new(spec(
            "chainsync_duration_micros",
            "ChainSync operation duration in microseconds",
        )),
        coverage: gauge_vec(
            spec("chainsync_coverage", "Logical subscriptions by coverage"),
            &["coverage"],
        ),
        losses: counter_vec(
            spec(
                "chainsync_coverage_losses",
                "Unexpected final-source losses",
            ),
            &["source"],
        ),
        activation_failures: counter(
            spec(
                "chainsync_activation_failures",
                "Entries into activation rescue",
            ),
            0,
        ),
        rescues: counter_vec(
            spec(
                "chainsync_rescues",
                "Rescue decisions and scheduling results",
            ),
            &["outcome"],
        ),
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

/// Updates gauges when a subscription changes between `ws_only`, `grpc_only`, and `both`.
/// `None` means no confirmed coverage; unchanged states do not update gauges.
pub(crate) fn coverage(before: Option<&'static str>, after: Option<&'static str>) {
    if before == after {
        return;
    }
    with_metrics(&METRICS, |metrics| {
        if let Some(before) = before {
            metrics.coverage.with_label_values(&[before]).dec();
        }
        if let Some(after) = after {
            metrics.coverage.with_label_values(&[after]).inc();
        }
    });
}

/// Counts an unexpected final-source loss, not an intentional removal.
pub(crate) fn lost(source: Transport) {
    with_metrics(&METRICS, |metrics| {
        metrics.losses.with_label_values(&[source.label()]).inc()
    });
}

pub(crate) fn activation_failed() {
    with_metrics(&METRICS, |metrics| metrics.activation_failures.inc());
}

/// Counts a `scheduled`, `superseded`, or `failed` rescue outcome.
/// Scheduling success means Engine accepted materialization, not on-chain undelegation.
pub(crate) fn rescue(outcome: &'static str) {
    with_metrics(&METRICS, |metrics| {
        metrics.rescues.with_label_values(&[outcome]).inc()
    });
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
