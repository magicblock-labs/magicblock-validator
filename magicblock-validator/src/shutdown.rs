use magicblock_core::shutdown::{ShutdownHandle, ShutdownReason};
use tokio::signal;
#[cfg(unix)]
use tokio::signal::unix::SignalKind;
use tracing::{error, info, instrument};

pub struct Shutdown;
impl Shutdown {
    #[cfg(unix)]
    #[instrument]
    pub async fn wait() -> std::io::Result<&'static str> {
        let mut terminate_signal =
            signal::unix::signal(SignalKind::terminate())?;
        let signal = tokio::select! {
            _ = terminate_signal.recv() => {
                info!(signal = "SIGTERM", "Initiating graceful shutdown");
                "SIGTERM"
            },
            _ = tokio::signal::ctrl_c() => {
                info!(signal = "SIGINT", "Initiating graceful shutdown");
                "SIGINT"
            },
        };

        Ok(signal)
    }

    #[cfg(not(unix))]
    pub async fn wait() -> std::io::Result<&'static str> {
        tokio::signal::ctrl_c().await?;
        info!(signal = "SIGINT", "Initiating graceful shutdown");
        Ok("SIGINT")
    }
}

/// Waits until an operator signal or a fatal shutdown request is received.
#[instrument(skip(shutdown))]
pub async fn wait_for_shutdown(shutdown: &ShutdownHandle) {
    tokio::select! {
        res = Shutdown::wait() => {
            match res {
                Ok(signal) => {
                    shutdown.trigger(ShutdownReason::Signal(signal));
                }
                Err(err) => {
                    error!(error = ?err, "Failed to wait for shutdown signal");
                    shutdown.trigger(ShutdownReason::Fatal {
                        source: "validator.signal",
                        message: format!("Failed to wait for shutdown signal: {err}"),
                    });
                }
            }
        }
        _ = shutdown.requested() => {
            if let Some(reason) = shutdown.reason() {
                error!(?reason, "Shutdown requested");
            }
        }
    }
}
