//! Test utilities for magicblock-committor-service tests

/// Initialize logging for tests. Call this at the start of each test
/// to enable structured logging via tracing.
///
/// This respects the RUST_LOG environment variable.
/// Example: `RUST_LOG=trace cargo test`
pub fn init_test_logger() {
    // Repeated across tests in a process, so failures to install are expected
    // and ignored: the first caller wins.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_test_writer()
        .try_init();
}
