use async_trait::async_trait;
use magicblock_accounts::{errors::AccountsResult, ScheduledCommitsProcessor};

#[derive(Default)]
pub struct ScheduledCommitsProcessorStub {}

#[async_trait]
impl ScheduledCommitsProcessor for ScheduledCommitsProcessorStub {
    async fn process(&self) -> AccountsResult<()> {
        Ok(())
    }
    fn scheduled_commits_len(&self) -> usize {
        0
    }
    fn clear_scheduled_commits(&self) {}
}
