pub(crate) mod db;
pub mod intent_channerl;
mod intent_execution_engine;
pub mod intent_scheduler;

use std::sync::{Arc, Mutex};

pub use intent_execution_engine::BroadcastedIntentExecutionResult;
use magicblock_core::traits::ActionsCallbackScheduler;
use magicblock_program::outbox_intent_bundles::OutboxIntentBundle;
use magicblock_rpc_client::MagicblockRpcClient;
use magicblock_table_mania::TableMania;
use tokio::sync::broadcast;

use crate::{
    intent_engine::{
        db::DB,
        intent_channerl::{channel, IntentScheduleError, IntentScheduleHandle},
        intent_execution_engine::{IntentExecutionEngine, ResultSubscriber},
    },
    intent_executor::{
        error::IntentExecutorError,
        intent_executor_factory::{ExecutorConfig, IntentExecutorBuilderImpl},
    },
    outbox_client::OutboxClient,
    tasks::task_info_fetcher::{CacheTaskInfoFetcher, RpcTaskInfoFetcher},
};

pub struct IntentEngineHandle<D: DB> {
    intent_schedule_handle: IntentScheduleHandle<D>,
    result_subscriber: ResultSubscriber,
}

impl<D: DB> IntentEngineHandle<D> {
    pub fn new<A, O>(
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
            rpc_client,
            table_mania,
            executor_config,
            outbox_client,
            task_info_fetcher,
            actions_callback_executor,
        };

        let (handle, intent_stream) = channel(&db, 1000);
        let worker =
            IntentExecutionEngine::new(intent_stream, executor_factory);
        let result_subscriber = worker.spawn();

        Self {
            intent_schedule_handle: handle,
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
        self.intent_schedule_handle.schedule(intent_bundles)
    }

    /// Creates a subscription for results of BaseIntent execution
    pub fn subscribe_for_results(
        &self,
    ) -> broadcast::Receiver<BroadcastedIntentExecutionResult> {
        self.result_subscriber.subscribe()
    }
}
