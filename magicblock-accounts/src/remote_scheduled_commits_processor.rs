use std::sync::Arc;

use magicblock_accounts_api::InternalAccountProvider;
use magicblock_committor_service::CommittorService;

use crate::errors::AccountsResult;

struct RemoteScheduledCommitsProcessor {
    committer_service: Arc<CommittorService>,
}

impl RemoteScheduledCommitsProcessor {
    pub fn new(committer_service: Arc<CommittorService>) -> Self {
        Self { committer_service }
    }

    async fn process<IAP>(&self, account_provider: &IAP) -> AccountsResult<()>
    where
        IAP: InternalAccountProvider,
    {
        todo!()
    }
}
