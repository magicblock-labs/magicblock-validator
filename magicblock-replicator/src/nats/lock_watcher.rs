//! Lock watcher for detecting leader expiration.

use async_nats::jetstream::kv::{Operation, Watch};
use futures::StreamExt;
use tokio_util::sync::CancellationToken;
use tracing::warn;

use super::cfg;
use crate::nats::Broker;

/// Watches the leader lock for expiration/deletion.
///
/// Used by standby nodes to detect when the primary's lock expires,
/// enabling faster takeover than waiting for the activity timeout.
pub struct LockWatcher {
    watch: Box<Watch>,
}

impl LockWatcher {
    /// Creates a new lock watcher.
    pub(crate) async fn new(
        broker: &Broker,
        cancel: &CancellationToken,
    ) -> Option<Self> {
        let watch = loop {
            if cancel.is_cancelled() {
                return None;
            }
            let store = match broker.ctx.get_key_value(cfg::PRODUCER_LOCK).await
            {
                Ok(s) => s,
                Err(error) => {
                    tracing::error!(%error, "failed to obtain lock object");
                    continue;
                }
            };
            match store.watch(cfg::LOCK_KEY).await {
                Ok(w) => break Box::new(w),
                Err(error) => {
                    tracing::error!(%error, "failed to create lock watcher");
                    continue;
                }
            }
        };
        Some(Self { watch })
    }

    /// Waits for the lock to be deleted or expire.
    ///
    /// Returns when the lock key is deleted or purged (TTL expiry).
    /// This signals that a takeover attempt should be made.
    pub async fn wait_for_expiry(&mut self) {
        while let Some(result) = self.watch.next().await {
            let operation = match result {
                Ok(entry) => entry.operation,
                Err(e) => {
                    warn!(%e, "lock watch error");
                    continue;
                }
            };
            if matches!(operation, Operation::Delete | Operation::Purge) {
                return;
            }
        }
        warn!("lock watch stream ended unexpectedly");
    }
}
