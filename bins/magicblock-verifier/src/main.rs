use anyhow::{Context, Result};
use engine::Engine;
use magicblock_config::VerifierParams;
use nucleus::shutdown::{ShutdownManager, ShutdownReason};
use replicator::ReplicationClient;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::info;

fn init_logger() {
    use magicblock_core::logger::{LogStyle, LoggingConfig, init_with_config};
    init_with_config(LoggingConfig {
        style: LogStyle::from_env(),
    });
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    init_logger();
    let config = VerifierParams::try_new(std::env::args_os())
        .map_err(|error| anyhow::anyhow!(error.to_string()))
        .context("failed to load verifier config")?;
    info!(config = ?config, "starting verifier");

    let metrics_token = CancellationToken::new();
    let _metrics = magicblock_metrics::try_start_metrics_service(
        config.metrics.address.0,
        metrics_token.clone(),
    )
    .context("failed to start metrics service")?;
    let _metrics_guard = metrics_token.drop_guard();

    loop {
        let restart = run(&config).await?;
        if !restart {
            return Ok(());
        }
        info!("restarting the verifier");
    }
}

async fn run(config: &VerifierParams) -> Result<bool> {
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

    let reason = shutdown.wait().await;
    shutdown.terminate().await;
    drop(engine);
    let restart = matches!(reason, ShutdownReason::RestartRequired);
    Ok(restart)
}
