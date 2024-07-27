use std::sync::Arc;

use async_trait::async_trait;
use sleipnir_accounts::{
    errors::AccountsResult, AccountCommitter, InternalAccountProvider,
    ScheduledCommitsProcessor,
};

#[derive(Default)]
pub struct ScheduledCommitsProcessorStub {}

#[async_trait]
impl ScheduledCommitsProcessor for ScheduledCommitsProcessorStub {
    async fn process<AC: AccountCommitter, IAP: InternalAccountProvider>(
        &self,
        _committer: &Arc<AC>,
        _account_provider: &IAP,
    ) -> AccountsResult<()> {
        Ok(())
    }
}
