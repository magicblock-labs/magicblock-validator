use std::io;

use log::info;
use tokio::{signal, signal::unix::SignalKind};

pub struct Shutdown;
impl Shutdown {
    pub async fn wait() -> Result<(), io::Error> {
        #[cfg(unix)]
        return Self::wait_unix().await;
        #[cfg(not(unix))]
        return Self::wait_other().await;
    }

    #[cfg(unix)]
    async fn wait_unix() -> Result<(), io::Error> {
        let mut terminate_signal =
            signal::unix::signal(SignalKind::terminate())?;
        tokio::select! {
            _ = terminate_signal.recv() => {
                info!("SIGTERM has been received, initiating graceful shutdown");
            },
            _ = tokio::signal::ctrl_c() => {
                info!("ctr-c signal received, initiating graceful shutdown");
            },
        }

        Ok(())
    }

    #[cfg(not(unix))]
    async fn wait_other() -> Result<(), io::Error> {
        tokio::signal::ctrl_c().await
    }
}
