use std::process::ExitCode;

use anyhow::{Context, Result};
use engine::Engine;
use magicblock_config::{ConfigError, VerifierParams};
use nucleus::shutdown::{Service, ShutdownManager, ShutdownReason};
use replicator::ReplicationClient;
use tokio::sync::mpsc;
use tracing::{error, info};

fn init_logger() {
    use magicblock_core::logger::{LogStyle, LoggingConfig, init_with_config};
    init_with_config(LoggingConfig {
        style: LogStyle::from_env(),
    });
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    init_logger();
    let reason = run()
        .await
        .unwrap_or_else(|error| ShutdownReason::Error(error.into()));
    exit(reason)
}

async fn run() -> Result<ShutdownReason> {
    let Some(config) = load_config()? else {
        return Ok(ShutdownReason::Signalled);
    };
    info!(config = ?config, "starting verifier");

    let metrics =
        magicblock_metrics::MetricsService::bind(config.metrics.address.0)
            .await
            .context("failed to bind metrics service")?;
    let mut metrics_shutdown = ShutdownManager::default();
    let shutdown = metrics_shutdown.handle(Service::Metrics);
    tokio::spawn(metrics.run(shutdown));

    loop {
        let reason = run_engine(&config, &mut metrics_shutdown).await?;
        if !matches!(&reason, ShutdownReason::RestartRequired) {
            return Ok(reason.combine(metrics_shutdown.terminate().await));
        }
        info!("restarting the verifier");
    }
}

fn load_config() -> Result<Option<VerifierParams>> {
    match VerifierParams::try_new(std::env::args_os()) {
        Ok(config) => Ok(Some(config)),
        Err(ConfigError::Cli(error)) if error.exit_code() == 0 => {
            error.print().context("failed to print verifier help")?;
            Ok(None)
        }
        Err(error) => Err(error).context("failed to load verifier config"),
    }
}

async fn run_engine(
    config: &VerifierParams,
    metrics_shutdown: &mut ShutdownManager,
) -> Result<ShutdownReason> {
    let mut shutdown = ShutdownManager::default();
    let builder =
        magicblock_runtime::keeper_builder(&config.engine, &config.programs)
            .context("failed to build verifier runtime image")?;
    let (pacer, blocks) = mpsc::channel(16);
    let engine = Engine::new(builder, Some(blocks), &mut shutdown)
        .await
        .context("failed to open verifier engine")?;
    ReplicationClient::spawn(
        config.engine.replication.upstream_address,
        engine.clone(),
        pacer,
        &mut shutdown,
    )
    .context("failed to start replication client")?;

    let reason = tokio::select! {
        biased;
        reason = metrics_shutdown.wait() => reason,
        reason = shutdown.wait() => reason,
    };
    let reason = reason.combine(shutdown.terminate().await);
    drop(engine);
    Ok(reason)
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
