use std::{error::Error as StdError, future::Future, time::Duration};

use anyhow::{Context, Error, Result};
use futures_util::{Stream, StreamExt};
use tokio::time::{Instant, timeout_at};
use tracing::info;

#[derive(Clone, Copy)]
pub(super) struct Deadline(Instant);

impl Deadline {
    pub(super) fn new(timeout: Duration) -> Self {
        Self(Instant::now() + timeout)
    }

    pub(super) async fn run<T, E>(
        self,
        stage: &'static str,
        future: impl Future<Output = Result<T, E>>,
    ) -> Result<T>
    where
        E: StdError + Send + Sync + 'static,
    {
        info!(stage, "Starting healthcheck stage");
        timeout_at(self.0, future)
            .await
            .with_context(|| format!("healthcheck timed out while {stage}"))?
            .map_err(Error::new)
    }

    pub(super) async fn next<T>(
        self,
        stage: &'static str,
        stream: &mut (impl Stream<Item = T> + Unpin),
    ) -> Result<T> {
        info!(stage, "Starting healthcheck stage");
        timeout_at(self.0, stream.next())
            .await
            .with_context(|| format!("healthcheck timed out while {stage}"))?
            .with_context(|| format!("subscription closed while {stage}"))
    }
}
