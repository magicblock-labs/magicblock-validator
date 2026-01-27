use std::{
    fmt::{Debug, Display},
    sync::atomic::{AtomicU16, Ordering},
};

use tracing::*;

/// Logs a trace message with data and error until the max_trace count is reached,
/// at which point it logs a warning message with the error and count.
/// Subsequent calls reset the trace count.
/// # Arguments
/// * `trace_msg` - The message to log at trace level.
/// * `warn_msg` - The message to log at warn level.
/// * `data` - The data to include in the trace log.
/// * `err` - The error to include in both trace and warn logs.
/// * `max_trace` - The maximum number of trace logs before logging a warning.
/// * `trace_count` - An atomic counter tracking the number of trace logs.
pub fn log_trace_warn<T: Display, E: Debug>(
    trace_msg: &str,
    warn_msg: &str,
    data: &T,
    err: &E,
    max_trace: u16,
    trace_count: &AtomicU16,
) {
    if trace_count.fetch_add(1, Ordering::SeqCst) == max_trace {
        warn!(error = ?err, count = max_trace, warn_msg);
        trace_count.store(0, Ordering::SeqCst);
    } else {
        trace!(error = ?err, data = %data, trace_msg);
    }
}

pub fn log_trace_debug<T: Display, E: Debug>(
    trace_msg: &str,
    debug_msg: &str,
    data: &T,
    err: &E,
    max_trace: u16,
    trace_count: &AtomicU16,
) {
    if trace_count.fetch_add(1, Ordering::SeqCst) == max_trace {
        debug!(error = ?err, count = max_trace, debug_msg);
        trace_count.store(0, Ordering::SeqCst);
    } else {
        trace!(error = ?err, data = %data, trace_msg);
    }
}
