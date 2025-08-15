use log::info;
use tokio::{signal, signal::unix::SignalKind};

pub struct Shutdown;
impl Shutdown {
    #[cfg(unix)]
    pub async fn wait() -> std::io::Result<()> {
        let mut terminate_signal =
            signal::unix::signal(SignalKind::terminate())?;
        tokio::select! {
            _ = terminate_signal.recv() => {
                info!("SIGTERM has been received, initiating graceful shutdown");
            },
            _ = tokio::signal::ctrl_c() => {
                info!("SIGINT signal received, initiating graceful shutdown");
            },
        }

        Ok(())
    }

    #[cfg(not(unix))]
    pub async fn wait() -> std::io::Result<()> {
        tokio::signal::ctrl_c().await
    }
}
