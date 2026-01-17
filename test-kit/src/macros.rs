//! -----------------
//! helper test macros (copy paste from the old test-tools crate)
//! TODO(bmuddha): refactor as part of the tests redesign
//! -----------------

use std::{env, path::Path};

use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use tracing::instrument;
use tracing_log::LogTracer;
use tracing_subscriber::{fmt, EnvFilter};

/// Initialize logging for tests using tracing.
///
/// This sets up both LogTracer (to capture log crate logs) and the
/// tracing subscriber. It respects RUST_LOG for filtering and optionally
/// appends test file name based on RUST_LOG_STYLE.
pub fn init_logger_for_tests() {
    // Capture log records from dependencies using the `log` crate
    let _ = LogTracer::init();

    // Build the RUST_LOG filter
    let mut rust_log = env::var("RUST_LOG")
        .ok()
        .unwrap_or_else(|| "info".to_string());

    // Support RUST_LOG_STYLE to detect test file and append to RUST_LOG
    if let Ok(style_env) = env::var("RUST_LOG_STYLE") {
        if style_env.to_lowercase() == "test" {
            if let Ok(test_file) = env::var("TEST_FILE_PATH") {
                let p = Path::new(&test_file);
                if let Some(file_stem) = p.file_stem() {
                    if let Some(file_name) = file_stem.to_str() {
                        rust_log.push_str(&format!(",{}=debug", file_name));
                    }
                }
            }
        }
    }

    let env_filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(rust_log));

    let _ = fmt::Subscriber::builder()
        .with_env_filter(env_filter)
        .with_test_writer()
        .try_init();
}

/// Initialize logging for a test file using the test file path.
///
/// This is the legacy entry point that maintains backward compatibility.
/// It extracts the test file name and initializes the logger.
pub fn init_logger_for_test_path(full_path_to_test_file: &str) {
    // In order to include logs from the test themselves we need to add the
    // name of the test file (minus the extension) to the RUST_LOG filter
    let mut rust_log = env::var("RUST_LOG")
        .ok()
        .unwrap_or_else(|| "info".to_string());

    if rust_log.ends_with(',') || rust_log == "info" {
        let p = Path::new(full_path_to_test_file);
        if let Some(file_stem) = p.file_stem() {
            if let Some(file_name) = file_stem.to_str() {
                let test_level = env::var("RUST_LOG_LEVEL")
                    .ok()
                    .unwrap_or_else(|| "info".to_string());
                rust_log.push_str(&format!("{}={}", file_name, test_level));
            }
        }
    }

    // Capture log records from dependencies using the `log` crate
    let _ = LogTracer::init();

    let env_filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(rust_log));

    let _ = fmt::Subscriber::builder()
        .with_env_filter(env_filter)
        .with_test_writer()
        .try_init();
}

#[macro_export]
macro_rules! init_logger {
    () => {
        $crate::macros::init_logger_for_test_path(::std::file!());
    };
}

#[instrument]
pub async fn is_devnet_up() -> bool {
    RpcClient::new("https://api.devnet.solana.com".to_string())
        .get_version()
        .await
        .is_ok()
}

#[macro_export]
macro_rules! skip_if_devnet_down {
    () => {
        if !$crate::macros::is_devnet_up().await {
            ::tracing::warn!("Devnet is down, skipping test");
            return;
        }
    };
}
