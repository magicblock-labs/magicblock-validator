use tokio::signal;
#[cfg(unix)]
use tokio::signal::unix::SignalKind;
use tracing::{info, instrument};

pub struct Shutdown;
impl Shutdown {
    #[cfg(unix)]
    #[instrument]
    pub async fn wait() -> std::io::Result<()> {
        let mut terminate_signal =
            signal::unix::signal(SignalKind::terminate())?;
        tokio::select! {
            _ = terminate_signal.recv() => {
                info!(signal = "SIGTERM", "Initiating graceful shutdown");
            },
            _ = tokio::signal::ctrl_c() => {
                info!(signal = "SIGINT", "Initiating graceful shutdown");
            },
        }

        Ok(())
    }

    #[cfg(not(unix))]
    pub async fn wait() -> std::io::Result<()> {
        tokio::signal::ctrl_c().await
    }
}
