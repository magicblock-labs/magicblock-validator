//! Shared cutoff for the probe's RPC calls and subscription waits.

use std::{error::Error as StdError, future::Future, time::Duration};

use anyhow::{Context, Error, Result};
use futures_util::{Stream, StreamExt};
use tokio::time::{Instant, timeout_at};
use tracing::debug;

/// One absolute deadline: copying it or advancing stages does not renew the budget.
#[derive(Clone, Copy)]
pub(super) struct Deadline(Instant);

impl Deadline {
    /// Starts the end-to-end budget immediately.
    pub(super) fn new(timeout: Duration) -> Self {
        Self(Instant::now() + timeout)
    }

    /// Applies the shared cutoff, preserving RPC errors and identifying stages that time out.
    pub(super) async fn run<T, E>(
        self,
        stage: &'static str,
        future: impl Future<Output = Result<T, E>>,
    ) -> Result<T>
    where
        E: StdError + Send + Sync + 'static,
    {
        debug!(stage, "Healthcheck stage");
        timeout_at(self.0, future)
            .await
            .with_context(|| format!("healthcheck timed out while {stage}"))?
            .map_err(Error::new)
    }

    /// Waits for a stream item within the remaining budget; a closed stream fails the stage.
    pub(super) async fn next<T>(
        self,
        stage: &'static str,
        stream: &mut (impl Stream<Item = T> + Unpin),
    ) -> Result<T> {
        debug!(stage, "Healthcheck stage");
        timeout_at(self.0, stream.next())
            .await
            .with_context(|| format!("healthcheck timed out while {stage}"))?
            .with_context(|| format!("subscription closed while {stage}"))
    }
}
