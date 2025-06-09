use std::{sync::Arc, time::Duration};

use crate::ChangesetCommittor;

pub struct CommittorRetryServiceConfig {
    /// The frequency at which the retry service will check for commits that need
    /// to be retried.
    /// If `None`, the service will not retry commits automatically and is triggered
    /// manually instead.
    pub frequency: Option<Duration>,
}

pub struct CommittorRetryService<CC: ChangesetCommittor> {
    committor: Arc<CC>,
    config: CommittorRetryServiceConfig,
}

impl<CC: ChangesetCommittor> CommittorRetryService<CC> {
    pub fn new(
        committor: Arc<CC>,
        config: CommittorRetryServiceConfig,
    ) -> Self {
        Self { committor, config }
    }

    pub async fn retry_pending(&self) {
        // self.committor.get_commit_statuses().await?;
    }
}
