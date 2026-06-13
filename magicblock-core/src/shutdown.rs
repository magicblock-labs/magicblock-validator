use std::sync::{Arc, OnceLock};

use arc_swap::ArcSwapOption;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::error;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ShutdownReason {
    /// Operator-initiated shutdown (SIGTERM / Ctrl-C).
    Signal(&'static str),
    /// A subsystem hit an unrecoverable error.
    Fatal {
        source: &'static str,
        message: String,
    },
}

/// Process-wide shutdown coordinator.
///
/// Owns the master [`CancellationToken`] shared by validator subsystems and
/// records the first shutdown reason.
#[derive(Clone)]
pub struct ShutdownHandle {
    token: CancellationToken,
    reason: Arc<ArcSwapOption<ShutdownReason>>,
}

impl ShutdownHandle {
    pub fn new() -> Self {
        Self {
            token: CancellationToken::new(),
            reason: Arc::new(ArcSwapOption::empty()),
        }
    }

    /// Requests graceful shutdown. Only the first reason is retained.
    pub fn trigger(&self, reason: ShutdownReason) {
        if self
            .reason
            .compare_and_swap(
                &None::<Arc<ShutdownReason>>,
                Some(Arc::new(reason.clone())),
            )
            .is_some()
        {
            self.token.cancel();
            return;
        }
        error!(?reason, "Shutdown requested");
        self.token.cancel();
    }

    pub fn is_triggered(&self) -> bool {
        self.token.is_cancelled()
    }

    pub fn reason(&self) -> Option<ShutdownReason> {
        self.reason.load_full().map(|reason| (*reason).clone())
    }

    pub fn token(&self) -> CancellationToken {
        self.token.clone()
    }

    pub fn child_token(&self) -> CancellationToken {
        self.token.child_token()
    }

    pub async fn requested(&self) {
        self.token.cancelled().await;
    }
}

impl Default for ShutdownHandle {
    fn default() -> Self {
        Self::new()
    }
}

static GLOBAL: OnceLock<ShutdownHandle> = OnceLock::new();

/// Installs the process-wide shutdown handle once at validator startup.
pub fn init_global(handle: ShutdownHandle) {
    let _ = GLOBAL.set(handle);
}

/// Requests shutdown from anywhere in the process after [`init_global`].
pub fn trigger_shutdown(reason: ShutdownReason) {
    if let Some(handle) = GLOBAL.get() {
        handle.trigger(reason);
    } else {
        error!(?reason, "Shutdown requested before coordinator init");
    }
}

/// Returns the process-wide shutdown handle if installed.
pub fn global() -> Option<ShutdownHandle> {
    GLOBAL.get().cloned()
}

/// Spawns a task that must run for the validator's whole lifetime.
///
/// If the task returns or panics while shutdown has not yet been requested, a
/// fatal shutdown is triggered.
pub fn spawn_critical<F>(
    name: &'static str,
    shutdown: ShutdownHandle,
    fut: F,
) -> JoinHandle<()>
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    tokio::spawn(async move {
        let inner = tokio::spawn(fut);
        match inner.await {
            Ok(()) => {
                if !shutdown.is_triggered() {
                    shutdown.trigger(ShutdownReason::Fatal {
                        source: name,
                        message: format!(
                            "critical task '{name}' exited unexpectedly"
                        ),
                    });
                }
            }
            Err(join_err) => {
                if shutdown.is_triggered() {
                    return;
                }
                let message = if join_err.is_panic() {
                    format!("critical task '{name}' panicked")
                } else {
                    format!("critical task '{name}' was cancelled")
                };
                shutdown.trigger(ShutdownReason::Fatal {
                    source: name,
                    message,
                });
            }
        }
    })
}

/// Requests validator shutdown for an unrecoverable subsystem error.
pub fn request_fatal_shutdown(
    source: &'static str,
    message: impl Into<String>,
    shutdown: Option<&ShutdownHandle>,
) {
    let message = message.into();
    if let Some(handle) = shutdown {
        handle.trigger(ShutdownReason::Fatal { source, message });
    } else if let Some(handle) = global() {
        handle.trigger(ShutdownReason::Fatal { source, message });
    } else {
        error!(
            source,
            %message,
            "Fatal committor error without shutdown coordinator"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn first_reason_wins() {
        let shutdown = ShutdownHandle::new();
        shutdown.trigger(ShutdownReason::Signal("SIGTERM"));
        shutdown.trigger(ShutdownReason::Fatal {
            source: "test",
            message: "later".into(),
        });

        assert_eq!(shutdown.reason(), Some(ShutdownReason::Signal("SIGTERM")));
    }

    #[tokio::test]
    async fn requested_resolves_on_trigger() {
        let shutdown = ShutdownHandle::new();
        let waiter = shutdown.clone();
        let trigger = shutdown.clone();

        let wait_task = tokio::spawn(async move {
            waiter.requested().await;
            waiter.reason()
        });

        trigger.trigger(ShutdownReason::Signal("SIGINT"));
        assert_eq!(
            wait_task.await.unwrap(),
            Some(ShutdownReason::Signal("SIGINT"))
        );
    }

    #[tokio::test]
    async fn child_token_cancels_on_master_trigger() {
        let shutdown = ShutdownHandle::new();
        let child = shutdown.child_token();
        let child_wait = tokio::spawn(async move {
            child.cancelled().await;
        });

        shutdown.trigger(ShutdownReason::Signal("SIGTERM"));
        child_wait.await.unwrap();
    }

    #[tokio::test]
    async fn spawn_critical_triggers_on_unexpected_exit() {
        let shutdown = ShutdownHandle::new();
        let _handle = spawn_critical("test.task", shutdown.clone(), async {});
        shutdown.requested().await;
        assert_eq!(
            shutdown.reason(),
            Some(ShutdownReason::Fatal {
                source: "test.task",
                message: "critical task 'test.task' exited unexpectedly".into(),
            })
        );
    }

    #[test]
    fn request_fatal_shutdown_uses_explicit_handle() {
        let shutdown = ShutdownHandle::new();
        request_fatal_shutdown("committor.test", "boom", Some(&shutdown));
        assert_eq!(
            shutdown.reason(),
            Some(ShutdownReason::Fatal {
                source: "committor.test",
                message: "boom".into(),
            })
        );
    }

    #[test]
    fn request_fatal_shutdown_falls_back_to_global() {
        let shutdown = ShutdownHandle::new();
        init_global(shutdown.clone());
        request_fatal_shutdown("committor.test", "via global", None);
        assert_eq!(
            shutdown.reason(),
            Some(ShutdownReason::Fatal {
                source: "committor.test",
                message: "via global".into(),
            })
        );
    }
}
