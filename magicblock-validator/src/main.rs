mod shutdown;

use magicblock_api::{ledger, magic_validator::MagicValidator};
use magicblock_config::ValidatorParams;
use solana_signer::Signer;
use tokio::runtime::Builder;
use tracing::{debug, error, info, instrument};

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

    // Only log shutdown message when not using TUI (TUI will have restored terminal)
    #[cfg(not(feature = "tui"))]
    info!("main runtime shutdown!");
}

#[instrument(skip_all)]
async fn run() {
    // Initialize logger only when TUI is disabled
    // (TUI sets up its own tracing layer for log capture)
    #[cfg(not(feature = "tui"))]
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

    #[cfg(not(feature = "tui"))]
    info!(config = %format!("{config:#?}"), "Starting validator");

    const WS_PORT_OFFSET: u16 = 1;
    let rpc_port = config.aperture.listen.port();
    let ws_port = rpc_port + WS_PORT_OFFSET;
    let rpc_host = config.aperture.listen.ip();
    let validator_identity = config.validator.keypair.pubkey();
    let remote_rpc_url = config.rpc_url().to_string();
    #[cfg(feature = "tui")]
    let block_time_ms = config.ledger.block_time_ms();
    #[cfg(feature = "tui")]
    let lifecycle_mode = format!("{:?}", config.lifecycle);
    #[cfg(feature = "tui")]
    let base_fee = config.validator.basefee;

    debug!(rpc_port, ws_port, "Validator configured");

    let mut api = match MagicValidator::try_from_config(config).await {
        Ok(api) => api,
        Err(error) => {
            eprintln!("Failed to create validator runtime: {error}");
            std::process::exit(1);
        }
    };

    debug!("Created API, starting things up");

    // Lock the ledger file to prevent concurrent access
    let mut ledger_lock = ledger::ledger_lockfile(api.ledger().ledger_path());
    let _ledger_write_guard =
        ledger::lock_ledger(api.ledger().ledger_path(), &mut ledger_lock);

    api.start().await.expect("Failed to start validator");

    // TUI mode: run the terminal user interface
    #[cfg(feature = "tui")]
    {
        use magicblock_tui::{run_tui, TuiConfig};

        let ledger_path = api
            .ledger()
            .ledger_path()
            .to_str()
            .unwrap_or("")
            .to_string();

        let version = magicblock_version::Version::default();

        let tui_config = TuiConfig {
            rpc_url: format!("http://{}:{}", rpc_host, rpc_port),
            ws_url: format!("ws://{}:{}", rpc_host, ws_port),
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

        let block_rx = api.block_update_rx();
        let tx_status_rx = api.transaction_status_rx();
        let cancel = api.cancellation_token().clone();

        if let Err(err) = run_tui(tui_config, block_rx, tx_status_rx, cancel).await {
            error!(error = ?err, "TUI error");
        }
    }

    // Non-TUI mode: print banner and wait for shutdown signal
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
        print_info(format!(
            "📡 RPC endpoint:       http://{}:{}",
            rpc_host, rpc_port
        ));
        print_info(format!(
            "🔌 WebSocket endpoint: ws://{}:{}",
            rpc_host, ws_port
        ));
        print_info(format!("🌐 Remote RPC:         {}", remote_rpc_url));
        print_info(format!("🖥️ Validator ID: {}", validator_identity));
        print_info(format!(
            "🗄️ Ledger location:    {}",
            api.ledger().ledger_path().to_str().unwrap_or("")
        ));
        print_info("-----------------------------------");
        print_info("Ready for connections!");
        print_info("");

        if let Err(err) = Shutdown::wait().await {
            error!(error = ?err, "Failed to gracefully shutdown");
        }
    }

    api.prepare_ledger_for_shutdown();
    api.stop().await;
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
