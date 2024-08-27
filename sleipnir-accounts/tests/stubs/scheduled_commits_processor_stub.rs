use std::sync::Arc;

use async_trait::async_trait;
use sleipnir_accounts::{
    errors::AccountsResult, AccountCommitter, InternalAccountProvider,
    ScheduledCommitsProcessor,
};
use sleipnir_program::traits::AccountsRemover;

#[derive(Default)]
pub struct ScheduledCommitsProcessorStub {}

#[async_trait]
impl ScheduledCommitsProcessor for ScheduledCommitsProcessorStub {
    async fn process<
        AC: AccountCommitter,
        IAP: InternalAccountProvider,
        ARE: AccountsRemover,
    >(
        &self,
        _committer: &Arc<AC>,
        _account_provider: &IAP,
        _accounts_remover: &ARE,
    ) -> AccountsResult<()> {
        Ok(())
    }
}
