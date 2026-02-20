#[cfg(not(feature = "tui"))]
mod shutdown;

use std::time::Instant;

use magicblock_api::{ledger, magic_validator::MagicValidator};
use magicblock_config::ValidatorParams;
#[cfg(feature = "tui")]
use magicblock_tui_client::{enrich_config_from_rpc, run_tui, TuiConfig};
use solana_signer::Signer;
use tokio::runtime::Builder;
#[cfg(not(feature = "tui"))]
use tracing::info;
use tracing::{debug, error, instrument};

#[cfg(not(feature = "tui"))]
use crate::shutdown::Shutdown;

#[cfg(not(feature = "tui"))]
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

    #[cfg(not(feature = "tui"))]
    info!("main runtime shutdown!");
}

#[instrument(skip_all)]
async fn run() {
    #[cfg(not(feature = "tui"))]
    let overall_start = Instant::now();
    #[cfg(not(feature = "tui"))]
    init_logger();
    #[cfg(feature = "tokio-console")]
    console_subscriber::init();
    let args = std::env::args_os();
    println!("cli args: {:?}", args);
    let config = match ValidatorParams::try_new(args) {
        Ok(c) => c,
        Err(err) => {
            eprintln!("Failed to read validator config: {err}");
            std::process::exit(1);
        }
    };
    #[cfg(not(feature = "tui"))]
    info!(config = %format!("{config:#?}"), "Starting validator");
    let rpc_url = config.aperture.listen.http();
    let ws_url = config.aperture.listen.websocket();
    let validator_identity = config.validator.keypair.pubkey();
    let remote_rpc_url = config.rpc_url().to_string();
    #[cfg(feature = "tui")]
    let block_time_ms = config.ledger.block_time_ms();
    #[cfg(feature = "tui")]
    let lifecycle_mode = format!("{:?}", config.lifecycle);
    #[cfg(feature = "tui")]
    let base_fee = config.validator.basefee;
    debug!(%rpc_url, %ws_url, "Validator configured");
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
    #[cfg(feature = "tui")]
    {
        let ledger_path = api
            .ledger()
            .ledger_path()
            .to_str()
            .unwrap_or("")
            .to_string();
        let version = magicblock_version::Version::default();

        let mut tui_config = TuiConfig {
            rpc_url: rpc_url.clone(),
            ws_url: ws_url.clone(),
            remote_rpc_url,
            validator_identity: validator_identity.to_string(),
            ledger_path,
            block_time_ms,
            lifecycle_mode,
            base_fee,
            help_url: "https://docs.magicblock.xyz".to_string(),
            version: version.to_string(),
            git_version: version.git_version.to_string(),
        };
        enrich_config_from_rpc(&mut tui_config).await;

        if let Err(err) = run_tui(tui_config).await {
            error!(error = ?err, "TUI error");
        }
    }

    #[cfg(not(feature = "tui"))]
    {
        let version = magicblock_version::Version::default();
        print_info("");
        print_info("🧙 Magicblock Validator is running! 🪄✦");
        print_info(format!(
            "🏷️ Validator version: {} (Git: {})",
            version, version.git_version
        ));
        print_info("-----------------------------------");
        print_info(format!("📡 RPC endpoint:       {}", rpc_url));
        print_info(format!("🔌 WebSocket endpoint: {}", ws_url));
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
    }

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
#[cfg(not(feature = "tui"))]
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
