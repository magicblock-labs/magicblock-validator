pub mod db;
pub mod intent_channel;
mod intent_execution_engine;
pub mod intent_scheduler;

use std::sync::{Arc, Mutex};

pub use intent_execution_engine::BroadcastedIntentExecutionResult;
use magicblock_core::traits::ActionsCallbackScheduler;
use magicblock_program::outbox_intent_bundles::OutboxIntentBundle;
use magicblock_rpc_client::MagicblockRpcClient;
use magicblock_table_mania::TableMania;
use solana_keypair::Keypair;
use tokio::sync::{
    broadcast,
    mpsc::{Sender, error::TrySendError},
};

use crate::{
    intent_engine::{
        db::BacklogDB,
        intent_channel::{IntentScheduleError, channel},
        intent_execution_engine::{IntentExecutionEngine, ResultSubscriber},
    },
    intent_executor::{
        error::IntentExecutorError,
        intent_executor_factory::{ExecutorConfig, IntentExecutorBuilderImpl},
    },
    outbox::OutboxClient,
    tasks::task_info_fetcher::{CacheTaskInfoFetcher, RpcTaskInfoFetcher},
};

const POISONED_BACKLOG_MSG: &str = "intent backlog mutex poisoned";

pub struct IntentEngineHandle<D> {
    db: Arc<Mutex<D>>,
    sender: Sender<OutboxIntentBundle>,
    result_subscriber: ResultSubscriber,
}

impl<D: BacklogDB> IntentEngineHandle<D> {
    #[allow(clippy::too_many_arguments)]
    pub fn new<A, O>(
        authority: Keypair,
        rpc_client: MagicblockRpcClient,
        db: D,
        task_info_fetcher: Arc<CacheTaskInfoFetcher<RpcTaskInfoFetcher>>,
        outbox_client: Arc<O>,
        table_mania: TableMania,
        executor_config: ExecutorConfig,
        actions_callback_executor: A,
    ) -> Self
    where
        A: ActionsCallbackScheduler,
        O: OutboxClient,
        O::Error: Into<IntentExecutorError>,
    {
        let db = Arc::new(Mutex::new(db));

        let executor_factory = IntentExecutorBuilderImpl {
            authority,
            rpc_client,
            table_mania,
            executor_config,
            outbox_client,
            task_info_fetcher,
            actions_callback_executor,
        };

        let (sender, intent_stream) = channel(&db, 1000);
        let worker =
            IntentExecutionEngine::new(intent_stream, executor_factory);
        let result_subscriber = worker.spawn();

        Self {
            db,
            sender,
            result_subscriber,
        }
    }

    /// Schedules [`ScheduledBaseIntent`] intent to be executed
    /// In case the channel is full we write intent to DB
    /// Intents will be extracted and handled in the [`IntentExecutionEngine`]
    pub async fn schedule(
        &self,
        intent_bundles: Vec<OutboxIntentBundle>,
    ) -> Result<(), IntentScheduleError> {
        let db = self.db.lock().expect(POISONED_BACKLOG_MSG);
        if !db.is_empty() {
            db.store_intent_bundles(intent_bundles)?;
            return Ok(());
        }

        let mut iter = intent_bundles.into_iter();
        // Treated as regular value not propagated lower
        #[allow(clippy::result_large_err)]
        let res = iter.try_for_each(|el| self.sender.try_send(el));
        match res {
            Ok(_) => Ok(()),
            Err(TrySendError::Closed(_)) => {
                Err(IntentScheduleError::ChannelClosed)
            }
            Err(TrySendError::Full(el)) => {
                let leftovers = std::iter::once(el).chain(iter).collect();
                db.store_intent_bundles(leftovers)
                    .map_err(IntentScheduleError::from)
            }
        }
    }

    /// Creates a subscription for results of BaseIntent execution
    pub fn subscribe_for_results(
        &self,
    ) -> broadcast::Receiver<BroadcastedIntentExecutionResult> {
        self.result_subscriber.subscribe()
    }
}
