//! Event producer with distributed lock for leader election.

use async_nats::jetstream::{
    kv::{CreateErrorKind, Store, UpdateErrorKind},
    Context,
};
use bytes::Bytes;
use tracing::warn;

use super::cfg;
use crate::Result;

/// Event producer with distributed lock for leader election.
///
/// Only one producer can hold the lock at a time, ensuring exactly one
/// primary publishes events. The lock has a TTL and must be refreshed
/// periodically to maintain leadership.
pub struct Producer {
    lock: Box<Store>,
    id: Bytes,
    revision: u64,
}

impl Producer {
    pub(crate) async fn new(id: &str, js: &Context) -> Result<Self> {
        Ok(Self {
            lock: Box::new(js.get_key_value(cfg::PRODUCER_LOCK).await?),
            id: id.to_owned().into_bytes().into(),
            revision: 0,
        })
    }

    /// Attempts to acquire the leader lock.
    ///
    /// Returns `true` if this producer became the leader.
    /// Returns `false` if another producer already holds the lock.
    pub async fn acquire(&mut self) -> Result<bool> {
        match self.lock.create(cfg::LOCK_KEY, self.id.clone()).await {
            Ok(rev) => {
                self.revision = rev;
                Ok(true)
            }
            Err(e) if e.kind() == CreateErrorKind::AlreadyExists => Ok(false),
            Err(e) => Err(e.into()),
        }
    }

    /// Refreshes the leader lock to prevent expiration.
    ///
    /// Returns `false` if we lost the lock (another producer took over).
    /// This typically indicates a network partition or slow refresh.
    pub async fn refresh(&mut self) -> Result<bool> {
        match self
            .lock
            .update(cfg::LOCK_KEY, self.id.clone(), self.revision)
            .await
        {
            Ok(rev) => {
                self.revision = rev;
                Ok(true)
            }
            Err(e) if e.kind() == UpdateErrorKind::WrongLastRevision => {
                Ok(false)
            }
            Err(e) => {
                warn!(%e, "lock refresh failed");
                Err(e.into())
            }
        }
    }

    /// Release the leader lock
    pub async fn release(&self) -> Result<()> {
        self.lock
            .delete_expect_revision(cfg::LOCK_KEY, Some(self.revision))
            .await
            .map_err(Into::into)
    }
}
