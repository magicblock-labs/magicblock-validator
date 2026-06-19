pub(crate) mod db;
mod intent_execution_engine;
pub mod intent_scheduler;

#[cfg(test)]
pub(crate) mod test_support;

use std::sync::Arc;

pub use intent_execution_engine::BroadcastedIntentExecutionResult;
use magicblock_core::traits::ActionsCallbackScheduler;
use magicblock_program::magic_scheduled_base_intent::ScheduledIntentBundle;
use magicblock_rpc_client::MagicblockRpcClient;
use magicblock_table_mania::TableMania;
use tokio::sync::{broadcast, mpsc, mpsc::error::TrySendError, watch};
use tokio_util::sync::CancellationToken;

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
    shutdown: CancellationToken,
    idle_rx: watch::Receiver<bool>,
}

impl<D: DB> IntentExecutionManager<D> {
    pub fn new<P, A>(
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
            rpc_client,
            table_mania,
            executor_config,
            task_info_fetcher,
            actions_callback_executor,
        };

        let (sender, receiver) = mpsc::channel(1000);
        let shutdown = CancellationToken::new();
        let (idle_tx, idle_rx) = watch::channel(false);
        let worker = IntentExecutionEngine::new(
            db.clone(),
            executor_factory,
            intent_persister,
            receiver,
        );
        let result_subscriber = worker.spawn(shutdown.clone(), idle_tx);

        Self {
            db,
            intent_sender: sender,
            result_subscriber,
            shutdown,
            idle_rx,
        }
    }

    /// Signals the engine to stop accepting new intents and waits until all
    /// in-flight executors finish and broadcast their results.
    pub async fn shutdown(&self) {
        self.shutdown.cancel();
        if *self.idle_rx.borrow() {
            return;
        }
        let _ = self.idle_rx.clone().wait_for(|idle| *idle).await;
    }

    pub fn is_shutting_down(&self) -> bool {
        self.shutdown.is_cancelled()
    }

    /// Schedules [`ScheduledBaseIntent`] intent to be executed
    /// In case the channel is full we write intent to DB
    /// Intents will be extracted and handled in the [`IntentExecutionEngine`]
    pub async fn schedule(
        &self,
        intent_bundles: Vec<ScheduledIntentBundle>,
    ) -> Result<(), IntentExecutionManagerError> {
        if self.is_shutting_down() {
            return Err(IntentExecutionManagerError::ShuttingDown);
        }

        // If db not empty push el-t there
        // This means that at some point channel got full
        // Worker first will clean-up channel, and then DB.
        // Pushing into channel would break order of commits
        if !self.db.is_empty() {
            self.db.store_intent_bundles(intent_bundles).await?;
            return Ok(());
        }

        let mut iter = intent_bundles.into_iter();
        // Treated as regular value not propagated lower
        #[allow(clippy::result_large_err)]
        let res = iter.try_for_each(|el| self.intent_sender.try_send(el));
        match res {
            Ok(_) => Ok(()),
            Err(TrySendError::Closed(_)) => {
                Err(IntentExecutionManagerError::ChannelClosed)
            }
            Err(TrySendError::Full(el)) => {
                let leftovers = std::iter::once(el).chain(iter).collect();
                self.db
                    .store_intent_bundles(leftovers)
                    .await
                    .map_err(IntentExecutionManagerError::from)
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

#[derive(thiserror::Error, Debug)]
pub enum IntentExecutionManagerError {
    #[error("Channel was closed")]
    ChannelClosed,
    #[error("Intent execution manager is shutting down")]
    ShuttingDown,
    #[error("DBError: {0}")]
    DBError(#[from] db::Error),
}

#[cfg(test)]
mod tests {
    use std::{sync::Arc, time::Duration};

    use solana_pubkey::pubkey;

    use super::*;
    use crate::intent_execution_manager::{
        db::DummyDB,
        intent_scheduler::create_test_intent,
        test_support::{self, MockIntentExecutorFactory},
        IntentExecutionManager,
    };

    fn new_for_test(factory: MockIntentExecutorFactory) -> Arc<IntentExecutionManager<DummyDB>> {
        Arc::new(test_support::new_test_intent_execution_manager(factory))
    }

    #[tokio::test]
    async fn test_manager_schedule_rejected_after_shutdown() {
        let manager = new_for_test(MockIntentExecutorFactory::new());
        manager.shutdown.cancel();

        let intent = create_test_intent(
            1,
            &[pubkey!("1111111111111111111111111111111111111111111")],
            false,
        );
        let err = manager.schedule(vec![intent]).await.unwrap_err();
        assert!(matches!(err, IntentExecutionManagerError::ShuttingDown));
    }

    #[tokio::test]
    async fn test_manager_shutdown_waits_for_in_flight_intent() {
        let manager = new_for_test(
            MockIntentExecutorFactory::new()
                .with_execution_delay(Duration::from_millis(200)),
        );
        let mut result_receiver = manager.subscribe_for_results();

        let intent = create_test_intent(
            1,
            &[pubkey!("1111111111111111111111111111111111111111111")],
            false,
        );
        manager.schedule(vec![intent]).await.unwrap();

        let shutdown_handle = {
            let manager = manager.clone();
            tokio::spawn(async move { manager.shutdown().await })
        };

        let result = tokio::time::timeout(
            Duration::from_secs(5),
            result_receiver.recv(),
        )
        .await
        .expect("timed out waiting for intent result")
        .expect("result channel closed");
        assert!(result.is_ok());
        assert_eq!(result.id, 1);

        shutdown_handle.await.expect("shutdown task panicked");
        assert!(manager.is_shutting_down());
    }

    #[tokio::test]
    async fn test_manager_shutdown_is_idempotent() {
        let manager = new_for_test(MockIntentExecutorFactory::new());

        manager.shutdown().await;
        manager.shutdown().await;

        assert!(manager.is_shutting_down());
    }
}
