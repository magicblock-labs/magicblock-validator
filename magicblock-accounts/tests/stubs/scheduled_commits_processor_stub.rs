use std::sync::Arc;

use async_trait::async_trait;
use magicblock_accounts::{errors::AccountsResult, ScheduledCommitsProcessor};
use magicblock_accounts_api::InternalAccountProvider;
use magicblock_committor_service::BaseIntentCommittor;

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
