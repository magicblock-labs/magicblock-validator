use async_trait::async_trait;
use log::*;
use sleipnir_program::TransactionScheduler;

use crate::{errors::AccountsResult, ScheduledCommitsProcessor};

pub struct RemoteScheduledCommitsProcessor {
    transaction_scheduler: TransactionScheduler,
}

impl RemoteScheduledCommitsProcessor {
    pub(crate) fn new() -> Self {
        Self {
            transaction_scheduler: TransactionScheduler::default(),
        }
    }
}

#[async_trait]
impl ScheduledCommitsProcessor for RemoteScheduledCommitsProcessor {
    async fn process(&self) -> AccountsResult<()> {
        let scheduled_commits =
            self.transaction_scheduler.take_scheduled_commits();
        for commit in scheduled_commits {
            info!("Processing commit: {:?}", commit);
        }
        Ok(())
    }
}
