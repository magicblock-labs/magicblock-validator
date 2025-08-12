pub(crate) mod db;
mod intent_execution_engine;
pub mod intent_scheduler;

use std::sync::Arc;

pub use intent_execution_engine::{
    BroadcastedIntentExecutionResult, ExecutionOutputWrapper,
};
use magicblock_rpc_client::MagicblockRpcClient;
use magicblock_table_mania::TableMania;
use tokio::sync::{broadcast, mpsc, mpsc::error::TrySendError};

use crate::{
    intent_execution_manager::{
        db::DB,
        intent_execution_engine::{IntentExecutionEngine, ResultSubscriber},
    },
    intent_executor::{
        commit_id_fetcher::CacheTaskInfoFetcher,
        intent_executor_factory::IntentExecutorFactoryImpl,
    },
    persist::IntentPersister,
    types::ScheduledBaseIntentWrapper,
    ComputeBudgetConfig,
};

pub struct IntentExecutionManager<D: DB> {
    db: Arc<D>,
    result_subscriber: ResultSubscriber,
    intent_sender: mpsc::Sender<ScheduledBaseIntentWrapper>,
}

impl<D: DB> IntentExecutionManager<D> {
    pub fn new<P: IntentPersister>(
        rpc_client: MagicblockRpcClient,
        db: D,
        intent_persister: Option<P>,
        table_mania: TableMania,
        compute_budget_config: ComputeBudgetConfig,
    ) -> Self {
        let db = Arc::new(db);

        let commit_id_tracker =
            Arc::new(CacheTaskInfoFetcher::new(rpc_client.clone()));
        let executor_factory = IntentExecutorFactoryImpl {
            rpc_client,
            table_mania,
            compute_budget_config,
            commit_id_tracker,
        };

        let (sender, receiver) = mpsc::channel(1000);
        let worker = IntentExecutionEngine::new(
            db.clone(),
            executor_factory,
            intent_persister,
            receiver,
        );
        let result_subscriber = worker.spawn();

        Self {
            db,
            intent_sender: sender,
            result_subscriber,
        }
    }

    /// Schedules [`ScheduledBaseIntent`] intent to be executed
    /// In case the channel is full we write intent to DB
    /// Intents will be extracted and handled in the [`IntentExecutionEngine`]
    pub async fn schedule(
        &self,
        base_intents: Vec<ScheduledBaseIntentWrapper>,
    ) -> Result<(), Error> {
        // If db not empty push el-t there
        // This means that at some point channel got full
        // Worker first will clean-up channel, and then DB.
        // Pushing into channel would break order of commits
        if !self.db.is_empty() {
            self.db.store_base_intents(base_intents).await?;
            return Ok(());
        }

        for el in base_intents {
            let err = if let Err(err) = self.intent_sender.try_send(el) {
                err
            } else {
                continue;
            };

            match err {
                TrySendError::Closed(_) => Err(Error::ChannelClosed),
                TrySendError::Full(el) => {
                    self.db.store_base_intent(el).await.map_err(Error::from)
                }
            }?;
        }

        Ok(())
    }

    /// Creates a subscription for results of BaseIntent execution
    pub fn subscribe_for_results(
        &self,
    ) -> broadcast::Receiver<BroadcastedIntentExecutionResult> {
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
