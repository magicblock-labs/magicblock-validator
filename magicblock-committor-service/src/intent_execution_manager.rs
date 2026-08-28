pub(crate) mod db;
mod intent_execution_engine;
pub mod intent_scheduler;

use std::sync::Arc;

pub use intent_execution_engine::BroadcastedIntentExecutionResult;
use magicblock_core::traits::ActionsCallbackScheduler;
use magicblock_program::magic_scheduled_base_intent::ScheduledIntentBundle;
use magicblock_rpc_client::MagicblockRpcClient;
use magicblock_table_mania::TableMania;
use solana_keypair::Keypair;
use tokio::sync::{broadcast, mpsc, mpsc::error::TrySendError};

use crate::{
    intent_execution_manager::{
        db::DB,
        intent_execution_engine::{IntentExecutionEngine, ResultSubscriber},
    },
    intent_executor::{
        intent_executor_factory::{ExecutorConfig, IntentExecutorFactoryImpl},
        task_info_fetcher::{CacheTaskInfoFetcher, RpcTaskInfoFetcher},
    },
    persist::IntentPersister,
};

pub struct IntentExecutionManager<D: DB> {
    db: Arc<D>,
    result_subscriber: ResultSubscriber,
    intent_sender: mpsc::Sender<ScheduledIntentBundle>,
}

impl<D: DB> IntentExecutionManager<D> {
    #[allow(clippy::too_many_arguments)]
    pub fn new<P, A>(
        authority: Keypair,
        rpc_client: MagicblockRpcClient,
        db: D,
        task_info_fetcher: Arc<CacheTaskInfoFetcher<RpcTaskInfoFetcher>>,
        intent_persister: Option<P>,
        table_mania: TableMania,
        executor_config: ExecutorConfig,
        actions_callback_executor: A,
    ) -> Self
    where
        A: ActionsCallbackScheduler,
        P: IntentPersister,
    {
        let db = Arc::new(db);

        let executor_factory = IntentExecutorFactoryImpl {
            authority,
            rpc_client,
            table_mania,
            executor_config,
            task_info_fetcher,
            actions_callback_executor,
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
        intent_bundles: Vec<ScheduledIntentBundle>,
    ) -> Result<(), IntentExecutionManagerError> {
        enqueue_intent_bundles(&self.intent_sender, self.db.as_ref(), intent_bundles)
            .await
    }

    /// Creates a subscription for results of BaseIntent execution
    pub fn subscribe_for_results(
        &self,
    ) -> broadcast::Receiver<BroadcastedIntentExecutionResult> {
        self.result_subscriber.subscribe()
    }
}

/// Try the live channel first so a later independent CAU can run while a
/// DummyDB ticket backlog drains. Overflow still goes to DummyDB. Same-account
/// order is preserved by [`IntentExecutionEngine::get_new_intent`].
pub(crate) async fn enqueue_intent_bundles<D: DB>(
    sender: &mpsc::Sender<ScheduledIntentBundle>,
    db: &D,
    intent_bundles: Vec<ScheduledIntentBundle>,
) -> Result<(), IntentExecutionManagerError> {
    let mut iter = intent_bundles.into_iter();
    // Treated as regular value not propagated lower
    #[allow(clippy::result_large_err)]
    let res = iter.try_for_each(|el| sender.try_send(el));
    match res {
        Ok(_) => Ok(()),
        Err(TrySendError::Closed(_)) => {
            Err(IntentExecutionManagerError::ChannelClosed)
        }
        Err(TrySendError::Full(el)) => {
            let leftovers = std::iter::once(el).chain(iter).collect();
            db.store_intent_bundles(leftovers)
                .await
                .map_err(IntentExecutionManagerError::from)
        }
    }
}

#[derive(thiserror::Error, Debug)]
pub enum IntentExecutionManagerError {
    #[error("Channel was closed")]
    ChannelClosed,
    #[error("DBError: {0}")]
    DBError(#[from] db::Error),
}
