use async_trait::async_trait;

use crate::errors::ScheduledCommitsProcessorResult;

#[async_trait]
pub trait ScheduledCommitsProcessor: Send + Sync + 'static {
    /// Processes all commits that were scheduled and accepted
    async fn process(&self) -> ScheduledCommitsProcessorResult<()>;

    /// Returns the number of commits that were scheduled and accepted
    fn scheduled_commits_len(&self) -> usize;

    /// Clears all scheduled commits
    fn clear_scheduled_commits(&self);

    /// Stop processor
    fn stop(&self);
}
