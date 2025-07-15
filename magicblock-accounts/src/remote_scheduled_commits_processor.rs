use std::sync::Arc;

use async_trait::async_trait;
use magicblock_bank::bank::Bank;
use magicblock_committor_service::L1MessageCommittor;
use magicblock_program::{
    magic_scheduled_l1_message::ScheduledL1Message, TransactionScheduler,
};
use magicblock_transaction_status::TransactionStatusSender;
use tokio::sync::mpsc::{channel, Sender};

use crate::{
    errors::AccountsResult,
    remote_scheduled_commits_worker::RemoteScheduledCommitsWorker,
    ScheduledCommitsProcessor,
};

pub struct RemoteScheduledCommitsProcessor<C: L1MessageCommittor> {
    transaction_scheduler: TransactionScheduler,
    bank: Arc<Bank>,
    worker_sender: Sender<Vec<ScheduledL1Message>>,
}

impl<C: L1MessageCommittor> RemoteScheduledCommitsProcessor<C> {
    pub fn new(
        bank: Arc<Bank>,
        committor: Arc<C>,
        transaction_status_sender: TransactionStatusSender,
    ) -> Self {
        let (worker_sender, worker_receiver) = channel(1000);
        let worker = RemoteScheduledCommitsWorker::new(
            bank.clone(),
            committor,
            transaction_status_sender,
            worker_receiver,
        );
        tokio::spawn(worker.start());

        Self {
            bank,
            worker_sender,
            transaction_scheduler: TransactionScheduler::default(),
        }
    }
}

#[async_trait]
impl<C: L1MessageCommittor> ScheduledCommitsProcessor
    for RemoteScheduledCommitsProcessor<C>
{
    async fn process(&self) -> AccountsResult<()> {
        let scheduled_l1_messages =
            self.transaction_scheduler.take_scheduled_actions();

        if scheduled_l1_messages.is_empty() {
            return Ok(());
        }

        self.worker_sender
            .send(scheduled_l1_messages)
            .await
            .expect("We shall be able to processs commmits");

        Ok(())
    }

    fn scheduled_commits_len(&self) -> usize {
        self.transaction_scheduler.scheduled_actions_len()
    }

    fn clear_scheduled_commits(&self) {
        self.transaction_scheduler.clear_scheduled_actions();
    }
}
