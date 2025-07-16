pub(crate) mod commit_scheduler_inner;
mod commit_scheduler_worker;
mod db;
mod executor_pool;

use std::sync::Arc;

use magicblock_program::magic_scheduled_l1_message::ScheduledL1Message;
use magicblock_rpc_client::MagicblockRpcClient;
use magicblock_table_mania::TableMania;
use tokio::sync::{broadcast, mpsc, mpsc::error::TrySendError};

use crate::{
    commit_scheduler::{
        commit_scheduler_worker::CommitSchedulerWorker, db::DB,
    },
    l1_message_executor::{ExecutionOutput, MessageExecutorResult},
    ComputeBudgetConfig,
};

pub struct CommitScheduler<D: DB> {
    db: Arc<D>,
    result_receiver:
        broadcast::Receiver<MessageExecutorResult<ExecutionOutput>>,
    message_sender: mpsc::Sender<ScheduledL1Message>,
}

impl<D: DB> CommitScheduler<D> {
    pub fn new(
        rpc_client: MagicblockRpcClient,
        db: D,
        table_mania: TableMania,
        compute_budget_config: ComputeBudgetConfig,
    ) -> Self {
        let db = Arc::new(db);
        let (sender, receiver) = mpsc::channel(1000);

        // TODO(edwin): add concellation logic
        let worker = CommitSchedulerWorker::new(
            db.clone(),
            rpc_client,
            table_mania,
            compute_budget_config,
            receiver,
        );
        let result_receiver = worker.spawn();

        Self {
            db,
            message_sender: sender,
            result_receiver,
        }
    }

    /// Schedules [`ScheduledL1Message`] message to be executed
    /// In case the channel is full we write message to DB
    /// Messages will be extracted and handled in the [`CommitSchedulerWorker`]
    pub async fn schedule(
        &self,
        l1_messages: Vec<ScheduledL1Message>,
    ) -> Result<(), Error> {
        for el in l1_messages {
            // If db not empty push el-t there
            // This means that at some point channel got full
            // Worker first will clean-up channel, and then DB.
            // Pushing into channel would break order of commits
            if !self.db.is_empty() {
                self.db.store_l1_messages(l1_messages).await?;
                continue;
            }

            let err = if let Err(err) = self.message_sender.try_send(el) {
                err
            } else {
                continue;
            };

            if matches!(err, TrySendError::Closed(_)) {
                Err(Error::ChannelClosed)
            } else {
                self.db
                    .store_l1_messages(l1_messages)
                    .await
                    .map_err(Error::from)
            }?
        }

        Ok(())
    }
}

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("Channel was closed")]
    ChannelClosed,
    #[error("DBError: {0}")]
    DBError(#[from] db::Error),
}
