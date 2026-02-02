mod shutdown;

use std::time::Instant;

use magicblock_api::{ledger, magic_validator::MagicValidator};
use magicblock_config::ValidatorParams;
use solana_signer::Signer;
use tokio::runtime::Builder;
use tracing::{debug, error, info, instrument};

use crate::shutdown::Shutdown;

fn init_logger() {
    use magicblock_core::logger::{init_with_config, LogStyle, LoggingConfig};
    let config = LoggingConfig {
        style: LogStyle::from_env(),
    };
    init_with_config(config);
}

fn main() {
    // We dedicate quarter of the threads to general async runtime (where io/timer
    // bound services are running), the rest is allocated for blocking io, rpc and
    // execution runtime (transaction scheduler/executor threads)
    let workers = (num_cpus::get() / 4).max(1);
    let runtime = Builder::new_multi_thread()
        .worker_threads(workers)
        .enable_all()
        .thread_name("async-runtime")
        .build()
        .expect("failed to build async runtime");
    runtime.block_on(run());
    drop(runtime);

    info!("main runtime shutdown!");
}

#[instrument(skip_all)]
async fn run() {
    let overall_start = Instant::now();
    init_logger();
    #[cfg(feature = "tokio-console")]
    console_subscriber::init();
    let args = std::env::args_os();
    let config = match ValidatorParams::try_new(args) {
        Ok(c) => c,
        Err(err) => {
            eprintln!("Failed to read validator config: {err}");
            std::process::exit(1);
        }
    };
    info!(config = %format!("{config:#?}"), "Starting validator");
    const WS_PORT_OFFSET: u16 = 1;
    let rpc_port = config.aperture.listen.port();
    let ws_port = rpc_port + WS_PORT_OFFSET; // WebSocket port is typically RPC port + 1
    let rpc_host = config.aperture.listen.ip();
    let validator_identity = config.validator.keypair.pubkey();
    let remote_rpc_url = config.rpc_url().to_string();
    debug!(rpc_port, ws_port, "Validator configured");
    let mut api = match MagicValidator::try_from_config(config).await {
        Ok(api) => api,
        Err(error) => {
            eprintln!("Failed to create validator runtime: {error}");
            std::process::exit(1);
        }
    };
    debug!("Created API, starting things up");
    // We need to create and hold on to the ledger lock here in order to keep the
    // underlying file locked while the app is running.
    // This prevents other processes from locking it until we exit.
    let mut ledger_lock = ledger::ledger_lockfile(api.ledger().ledger_path());
    let _ledger_write_guard =
        ledger::lock_ledger(api.ledger().ledger_path(), &mut ledger_lock);
    let start_step = Instant::now();
    api.start().await.expect("Failed to start validator");
    debug!(
        duration_ms = start_step.elapsed().as_millis() as u64,
        "Validator start completed"
    );
    let version = magicblock_version::Version::default();
    print_info("");
    print_info("🧙 Magicblock Validator is running! 🪄✦");
    print_info(format!(
        "🏷️ Validator version: {} (Git: {})",
        version, version.git_version
    ));
    print_info("-----------------------------------");
    print_info(format!(
        "📡 RPC endpoint:       http://{}:{}",
        rpc_host, rpc_port
    ));
    print_info(format!(
        "🔌 WebSocket endpoint: ws://{}:{}",
        rpc_host, ws_port
    ));
    print_info(format!("🌐 Remote RPC:         {}", remote_rpc_url));
    print_info(format!("🖥️ Validator identity: {}", validator_identity));
    print_info(format!(
        "🗄️ Ledger location:    {}",
        api.ledger().ledger_path().to_str().unwrap_or("")
    ));
    print_info("-----------------------------------");
    print_info("Ready for connections!");
    print_info("");
    debug!(
        duration_ms = overall_start.elapsed().as_millis() as u64,
        "Validator ready"
    );
    let shutdown_wait = Instant::now();
    if let Err(err) = Shutdown::wait().await {
        error!(error = ?err, "Failed to gracefully shutdown");
    }
    debug!(
        duration_ms = shutdown_wait.elapsed().as_millis() as u64,
        "Shutdown signal received"
    );

    api.prepare_ledger_for_shutdown();
    let stop_step = Instant::now();
    api.stop().await;
    debug!(
        duration_ms = stop_step.elapsed().as_millis() as u64,
        "Validator stop completed"
    );
}

/// Print informational startup messages.
/// - If `RUST_LOG` is not set or is set to "quiet", prints to stdout using `println!()`.
/// - Otherwise, emits an `info!` log so operators can control visibility
///   (e.g., by setting `RUST_LOG=warn` to hide it).
fn print_info<S: std::fmt::Display>(msg: S) {
    let rust_log = std::env::var("RUST_LOG").unwrap_or_default();
    let rust_log_trimmed = rust_log.trim().to_ascii_lowercase();
    let use_plain_print =
        rust_log_trimmed.is_empty() || rust_log_trimmed == "quiet";
    if use_plain_print {
        println!("{}", msg);
    } else {
        info!("{}", msg);
    }
}
