use std::{
    fmt::{Debug, Display},
    sync::atomic::{AtomicU16, Ordering},
};

use tracing::*;

/// Logs a warn message on the first invocation (prev_value == 0), then logs
/// trace-level messages for up to max_trace-1 calls. When the counter reaches
/// max_trace, emits a warn message. After each warn log, the trace_count is
/// reset to 1 for the next cycle.
/// # Arguments
/// * `trace_msg` - The message to log at trace level (used for intermediate calls).
/// * `warn_msg` - The message to log at warn level (used on first call and after max_trace).
/// * `data` - The data to include in trace logs.
/// * `err` - The error to include in both trace and warn logs.
/// * `max_trace` - The maximum number of times to log trace messages before logging warn.
/// * `trace_count` - An atomic counter tracking calls; reset to 1 after warn log.
pub fn log_trace_warn<T: Display, E: Debug>(
    trace_msg: &str,
    warn_msg: &str,
    data: &T,
    err: &E,
    max_trace: u16,
    trace_count: &AtomicU16,
) {
    let prev_value = trace_count.fetch_add(1, Ordering::SeqCst);
    // Log the first message and when the max_trace limit is reached
    if prev_value >= max_trace || prev_value == 0 {
        warn!(error = ?err, count = max_trace, warn_msg);
        // NOTE: 0 is reserved for the very first time this is invoked
        trace_count.store(1, Ordering::SeqCst);
    } else {
        trace!(error = ?err, data = %data, trace_msg);
    }
}

/// Logs a debug message on the first invocation (prev_value == 0), then logs
/// trace-level messages for up to max_trace-1 calls. When the counter reaches
/// max_trace, emits a debug message. After each debug log, the trace_count is
/// reset to 1 for the next cycle.
/// # Arguments
/// * `trace_msg` - The message to log at trace level (used for intermediate calls).
/// * `debug_msg` - The message to log at debug level (used on first call and after max_trace).
/// * `data` - The data to include in trace logs.
/// * `err` - The error to include in both trace and debug logs.
/// * `max_trace` - The maximum number of times to log trace messages before logging debug.
/// * `trace_count` - An atomic counter tracking calls; reset to 1 after debug log.
pub fn log_trace_debug<T: Display, E: Debug>(
    trace_msg: &str,
    debug_msg: &str,
    data: &T,
    err: &E,
    max_trace: u16,
    trace_count: &AtomicU16,
) {
    let prev_value = trace_count.fetch_add(1, Ordering::SeqCst);
    // Log the first message and when the max_trace limit is reached
    if prev_value >= max_trace || prev_value == 0 {
        debug!(error = ?err, count = max_trace, debug_msg);
        // NOTE: 0 is reserved for the very first time this is invoked
        trace_count.store(1, Ordering::SeqCst);
    } else {
        trace!(error = ?err, data = %data, trace_msg);
    }
}
