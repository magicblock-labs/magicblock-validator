use std::{future::Future, pin::Pin, sync::Arc, time::Duration};

use magicblock_core::traits::ActionsCallbackScheduler;
use magicblock_program::outbox_intent_bundles::{
    OutboxIntentBundle, OutboxIntentBundleStatus,
};
use magicblock_rpc_client::MagicblockRpcClient;
use magicblock_table_mania::TableMania;
use solana_keypair::Keypair;
use tracing::warn;

use crate::{
    ComputeBudgetConfig,
    intent_executor::{
        IntentExecutor, IntentExecutorCtx, build_stage_intent_executor,
        error::IntentExecutorError,
        intent_execution_client::IntentExecutionClient,
    },
    outbox::{
        OutboxClient, outbox_intent_bundles_reader::OutboxIntentBundlesReader,
    },
    tasks::task_info_fetcher::{CacheTaskInfoFetcher, RpcTaskInfoFetcher},
    transaction_preparator::TransactionPreparatorImpl,
};

pub type ReconcileIntentFuture<'a> =
    Pin<Box<dyn Future<Output = OutboxIntentBundle> + Send + 'a>>;

pub trait IntentExecutorBuilder<T> {
    fn create_instance(
        &self,
        status: OutboxIntentBundleStatus,
    ) -> Box<dyn IntentExecutor<T>>;

    fn reconcile_intent<'a>(
        &'a self,
        intent: &'a OutboxIntentBundle,
    ) -> ReconcileIntentFuture<'a>
    where
        Self: Sync,
    {
        Box::pin(async move { intent.clone() })
    }
}

pub struct ExecutorConfig {
    pub compute_budget_config: ComputeBudgetConfig,
    pub actions_timeout: Duration,
}

/// Dummy struct to simplify signature of CommitSchedulerWorker
pub struct IntentExecutorBuilderImpl<A, O> {
    /// Base-layer signing identity — the engine's authority keypair.
    pub authority: Keypair,
    pub rpc_client: MagicblockRpcClient,
    pub table_mania: TableMania,
    pub executor_config: ExecutorConfig,
    pub task_info_fetcher: Arc<CacheTaskInfoFetcher<RpcTaskInfoFetcher>>,
    pub outbox_client: Arc<O>,
    pub actions_callback_executor: A,
}

impl<A, O> IntentExecutorBuilder<TransactionPreparatorImpl>
    for IntentExecutorBuilderImpl<A, O>
where
    A: ActionsCallbackScheduler,
    O: OutboxClient,
    O::Error: Into<IntentExecutorError>,
{
    fn create_instance(
        &self,
        status: OutboxIntentBundleStatus,
    ) -> Box<dyn IntentExecutor<TransactionPreparatorImpl>> {
        let transaction_preparator = TransactionPreparatorImpl::new(
            self.rpc_client.clone(),
            self.table_mania.clone(),
            self.executor_config.compute_budget_config.clone(),
        );
        let ctx = IntentExecutorCtx {
            authority: self.authority.insecure_clone(),
            intent_client: IntentExecutionClient::new(self.rpc_client.clone()),
            transaction_preparator,
            task_info_fetcher: self.task_info_fetcher.clone(),
            outbox_client: self.outbox_client.clone(),
            actions_callback_executor: self.actions_callback_executor.clone(),
        };
        build_stage_intent_executor(
            ctx,
            status,
            self.executor_config.actions_timeout,
        )
    }

    fn reconcile_intent<'a>(
        &'a self,
        intent: &'a OutboxIntentBundle,
    ) -> ReconcileIntentFuture<'a> {
        Box::pin(async move {
            match self
                .outbox_client
                .outbox_reader()
                .fetch_outbox_intent(intent.id)
                .await
            {
                Ok(Some(bundle)) => bundle,
                Ok(None) => intent.clone(),
                Err(_) => {
                    warn!(
                        intent_id = intent.id,
                        "Failed to reconcile outbox intent before retry"
                    );
                    intent.clone()
                }
            }
        })
    }
}
