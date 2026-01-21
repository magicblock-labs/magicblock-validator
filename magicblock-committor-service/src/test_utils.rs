//! Test utilities for magicblock-committor-service tests

use test_kit::macros::init_logger_for_tests;

/// Initialize logging for tests. Call this at the start of each test
/// to enable structured logging via tracing.
///
/// This respects the RUST_LOG environment variable.
/// Example: `RUST_LOG=trace cargo test`
pub fn init_test_logger() {
    init_logger_for_tests();
}
