mod crank_faucet;
mod errors;
mod leader;
mod ledger;
mod magic_sys_adapter;

use std::process::ExitCode;

use anyhow::{Context, Result};
use leader::Leader;
use magicblock_config::{ConfigError, LeaderParams};
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

fn main() -> ExitCode {
    init_logger();
    let reason =
        try_main().unwrap_or_else(|error| ShutdownReason::Error(error.into()));
    exit(reason)
}

fn try_main() -> Result<ShutdownReason> {
    // RPC and other async services share this runtime. Use half the CPUs minus
    // one, leaving capacity for engine execution and blocking work.
    let workers = (num_cpus::get() / 2).saturating_sub(1).max(1);
    let runtime = Builder::new_multi_thread()
        .worker_threads(workers)
        .enable_all()
        .thread_name("async-runtime")
        .build()
        .context("failed to build leader async runtime")?;
    let result = runtime.block_on(run());
    info!("main runtime shutdown");
    result
}

#[instrument(skip_all)]
async fn run() -> Result<ShutdownReason> {
    let Some(config) = load_config()? else {
        return Ok(ShutdownReason::Signalled);
    };

    info!("Starting leader\n{config}");
    let rpc_url = config.aperture.listen.http();
    let ws_url = config.aperture.listen.websocket();
    let identity = config.engine.authority.local.pubkey().to_string();
    let remote_rpc_url = config.rpc_url().to_owned();

    let mut leader = Leader::try_from_config(config)
        .await
        .context("failed to create leader runtime")?;

    leader.start();

    print_startup(&rpc_url, &ws_url, &remote_rpc_url, &identity);
    Ok(leader.wait().await)
}

fn exit(reason: ShutdownReason) -> ExitCode {
    let code = reason.exit_code();
    if code == 0 {
        info!(exit_code = code, "process terminated cleanly");
    } else {
        error!(?reason, exit_code = code, "process terminated");
    }
    ExitCode::from(code)
}

fn load_config() -> Result<Option<LeaderParams>> {
    match LeaderParams::try_new(std::env::args_os()) {
        Ok(config) => Ok(Some(config)),
        Err(ConfigError::Cli(error)) if error.exit_code() == 0 => {
            error.print().context("failed to print leader help")?;
            Ok(None)
        }
        Err(error) => Err(error).context("failed to load leader config"),
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
