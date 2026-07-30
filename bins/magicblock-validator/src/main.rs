mod crank_faucet;
mod errors;
mod leader;
mod ledger;
mod magic_sys_adapter;

use leader::Leader;
use magicblock_config::LeaderParams;
use nucleus::shutdown::ShutdownReason;
use solana_signer::Signer;
use tokio::runtime::Builder;
use tracing::{error, info, instrument};

fn init_logger() {
    use magicblock_core::logger::{LogStyle, LoggingConfig, init_with_config};
    init_with_config(LoggingConfig {
        style: LogStyle::from_env(),
    });
}

fn main() {
    // Reserve most CPUs for blocking I/O, RPC, and engine execution.
    let workers = (num_cpus::get() / 4).max(1);
    let runtime = Builder::new_multi_thread()
        .worker_threads(workers)
        .enable_all()
        .thread_name("async-runtime")
        .build()
        .expect("failed to build async runtime");
    runtime.block_on(run());
    info!("main runtime shutdown");
}

#[instrument(skip_all)]
async fn run() {
    let config = match LeaderParams::try_new(std::env::args_os()) {
        Ok(config) => config,
        Err(error) => {
            eprintln!("Failed to read leader config: {error}");
            std::process::exit(1);
        }
    };
    init_logger();

    info!("Starting leader\n{config}");
    let rpc_url = config.aperture.listen.http();
    let ws_url = config.aperture.listen.websocket();
    let identity = config.engine.authority.local.pubkey().to_string();
    let remote_rpc_url = config.rpc_url().to_owned();

    let mut leader = match Leader::try_from_config(config).await {
        Ok(leader) => leader,
        Err(error) => {
            error!(%error, "Failed to create leader runtime");
            std::process::exit(1);
        }
    };

    if let Err(error) = leader.start().await {
        error!(%error, "Failed to start leader services");
        leader.stop().await;
        std::process::exit(1);
    }

    print_startup(&rpc_url, &ws_url, &remote_rpc_url, &identity);
    let cause = leader.shutdown.wait().await;
    let failed = !matches!(cause, ShutdownReason::Signalled);
    if failed {
        error!(?cause, "Engine service terminated before leader shutdown");
    }

    leader.stop().await;
    if failed {
        std::process::exit(1);
    }
}

fn print_startup(
    rpc_url: &str,
    ws_url: &str,
    remote_rpc_url: &str,
    identity: &str,
) {
    let version = magicblock_version::Version::default();
    for line in [
        String::new(),
        "🧙 MagicBlock leader is running! 🪄✦".to_owned(),
        format!("🏷️ Version: {} (Git: {})", version, version.git_version),
        "-----------------------------------".to_owned(),
        format!("📡 RPC endpoint:       {rpc_url}"),
        format!("🔌 WebSocket endpoint: {ws_url}"),
        format!("🌐 Remote RPC:         {remote_rpc_url}"),
        format!("🖥️ Leader identity:    {identity}"),
        "-----------------------------------".to_owned(),
        "Ready for connections!".to_owned(),
        String::new(),
    ] {
        print_info(line);
    }
}

fn print_info(message: impl std::fmt::Display) {
    let rust_log = std::env::var("RUST_LOG").unwrap_or_default();
    let rust_log = rust_log.trim().to_ascii_lowercase();
    if rust_log.is_empty() || rust_log == "quiet" {
        println!("{message}");
    } else {
        info!("{message}");
    }
}
