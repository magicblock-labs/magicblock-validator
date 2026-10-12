#![doc = include_str!("../README.md")]

mod errors;
mod leader;
mod ledger;
mod metrics;

use std::{alloc::System, process::ExitCode};

use anyhow::{Context, Result};
use config::{ConfigError, LeaderParams};
use leader::Leader;
use mbv_core::version::Version;
use nucleus::shutdown::ShutdownReason;
use tokio::runtime::Builder;
use tracing::{error, info, instrument};

// Geyser plugins must use System too: their allocations are freed by the validator.
#[global_allocator]
static ALLOCATOR: System = System;

fn init_logger() {
    use mbv_core::logger::{LogStyle, LoggingConfig, init_with_config};
    init_with_config(LoggingConfig { style: LogStyle::from_env() });
}

fn main() -> ExitCode {
    init_logger();
    let reason = try_main().unwrap_or_else(|error| ShutdownReason::Error(error.into()));
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

    let version = Version::default();
    info!(
        version = version.core,
        git = version.git_version,
        config = %config,
        "Starting leader"
    );

    let leader = Leader::start(config).await.context("failed to create leader runtime")?;

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
