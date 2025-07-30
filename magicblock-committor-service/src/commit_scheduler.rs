pub mod commit_id_tracker;
pub(crate) mod commit_scheduler_inner;
mod commit_scheduler_worker;
pub(crate) mod db; // TODO(edwin): define visibility

use std::sync::Arc;

pub use commit_scheduler_worker::{
    BroadcastedMessageExecutionResult, ExecutionOutputWrapper,
};
use magicblock_rpc_client::MagicblockRpcClient;
use magicblock_table_mania::TableMania;
use tokio::sync::{broadcast, mpsc, mpsc::error::TrySendError};

use crate::{
    commit_scheduler::{
        commit_id_tracker::CommitIdTrackerImpl,
        commit_scheduler_worker::{CommitSchedulerWorker, ResultSubscriber},
        db::DB,
    },
    message_executor::message_executor_factory::L1MessageExecutorFactory,
    persist::L1MessagesPersisterIface,
    types::ScheduledL1MessageWrapper,
    ComputeBudgetConfig,
};

pub struct CommitScheduler<D: DB> {
    db: Arc<D>,
    result_subscriber: ResultSubscriber,
    message_sender: mpsc::Sender<ScheduledL1MessageWrapper>,
}

impl<D: DB> CommitScheduler<D> {
    pub fn new<P: L1MessagesPersisterIface>(
        rpc_client: MagicblockRpcClient,
        db: D,
        l1_message_persister: Option<P>,
        table_mania: TableMania,
        compute_budget_config: ComputeBudgetConfig,
    ) -> Self {
        let db = Arc::new(db);

        let commit_id_tracker =
            Arc::new(CommitIdTrackerImpl::new(rpc_client.clone()));
        let executor_factory = L1MessageExecutorFactory {
            rpc_client,
            table_mania,
            compute_budget_config,
            commit_id_tracker,
        };

        let (sender, receiver) = mpsc::channel(1000);
        let worker = CommitSchedulerWorker::new(
            db.clone(),
            executor_factory,
            l1_message_persister,
            receiver,
        );
        // TODO(edwin): add concellation logic
        let result_subscriber = worker.spawn();

        Self {
            db,
            message_sender: sender,
            result_subscriber,
        }
    }

    /// Schedules [`ScheduledL1Message`] message to be executed
    /// In case the channel is full we write message to DB
    /// Messages will be extracted and handled in the [`CommitSchedulerWorker`]
    pub async fn schedule(
        &self,
        l1_messages: Vec<ScheduledL1MessageWrapper>,
    ) -> Result<(), Error> {
        // If db not empty push el-t there
        // This means that at some point channel got full
        // Worker first will clean-up channel, and then DB.
        // Pushing into channel would break order of commits
        if !self.db.is_empty() {
            self.db.store_l1_messages(l1_messages).await?;
            return Ok(());
        }

        for el in l1_messages {
            let err = if let Err(err) = self.message_sender.try_send(el) {
                err
            } else {
                continue;
            };

            match err {
                TrySendError::Closed(_) => Err(Error::ChannelClosed),
                TrySendError::Full(el) => {
                    self.db.store_l1_message(el).await.map_err(Error::from)
                }
            }?;
        }

        Ok(())
    }

    /// Creates a subscription for results of L1Message execution
    pub fn subscribe_for_results(
        &self,
    ) -> broadcast::Receiver<BroadcastedMessageExecutionResult> {
        self.result_subscriber.subscribe()
    }
}

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("Channel was closed")]
    ChannelClosed,
    #[error("DBError: {0}")]
    DBError(#[from] db::Error),
}
