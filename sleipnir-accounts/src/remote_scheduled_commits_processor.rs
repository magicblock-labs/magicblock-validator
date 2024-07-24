use std::sync::Arc;

use async_trait::async_trait;
use log::*;
use sleipnir_bank::bank::Bank;
use sleipnir_mutator::Cluster;
use sleipnir_program::{
    register_scheduled_commit_sent, SentCommit, TransactionScheduler,
};
use sleipnir_transaction_status::TransactionStatusSender;
use solana_sdk::{pubkey::Pubkey, signature::Signature};

use crate::{
    errors::AccountsResult, utils::accounts_execute_transaction,
    ScheduledCommitsProcessor,
};

pub struct RemoteScheduledCommitsProcessor {
    #[allow(unused)]
    cluster: Cluster,
    bank: Arc<Bank>,
    transaction_status_sender: Option<TransactionStatusSender>,
    transaction_scheduler: TransactionScheduler,
}

impl RemoteScheduledCommitsProcessor {
    pub(crate) fn new(
        cluster: Cluster,
        bank: Arc<Bank>,
        transaction_status_sender: Option<TransactionStatusSender>,
    ) -> Self {
        Self {
            cluster,
            bank,
            transaction_status_sender,
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
            let sent_commit = SentCommit::new(
                commit.id,
                commit.slot,
                commit.blockhash,
                commit.payer,
                vec![Signature::new_unique()],
                vec![Pubkey::new_unique()],
                vec![],
            );

            register_scheduled_commit_sent(sent_commit);

            let signature = accounts_execute_transaction(
                commit.commit_sent_transaction,
                &self.bank,
                self.transaction_status_sender.as_ref(),
            )?;
            debug!(
                "Commit sent transaction executed with signature: {:?}",
                signature
            );
        }
        Ok(())
    }
}
