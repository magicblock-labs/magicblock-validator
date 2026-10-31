use std::{
    sync::{Arc, atomic::AtomicU64},
    time::Duration,
};

use chainlink::ProdChainlink;
use engine::Engine;
use mbv_core::traits::ActionsCallbackScheduler;
use nucleus::shutdown::{ShutdownHandle, ShutdownReason};
use parking_lot::Mutex;
use rpc_client::MagicblockRpcClient;
use solana_keypair::Keypair;
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use table_mania::{GarbageCollectorConfig, TableMania};
use tokio::{sync::mpsc, task::JoinSet};
use tokio_util::{sync::CancellationToken, task::TaskTracker};

use crate::{
    config::CommittorConfig,
    execution::{
        ExecutionContext, IntentExecutor, backlog::AccountsDbIntentBacklog,
        client::IntentExecutionClient, preparation::Preparator, queue::IntentQueue,
        stream::IntentStream,
    },
    intake::{CommitIdProvider, worker::IntentIntake},
    shared::{
        account_state::{CachedTaskInfoProvider, RpcTaskInfoProvider},
        outbox::{
            outbox_client::InternalOutboxClient,
            outbox_intent_bundles_reader::OutboxIntentBundlesReaderError,
        },
    },
};

const SHUTDOWN_GRACE: Duration = Duration::from_secs(3);

/// Owns intake and execution. Only `run` starts background workers.
pub struct CommittorService {
    intake: IntentIntake,
    executor: IntentExecutor,
    context: Arc<ExecutionContext>,
    tables: TableMania,
}

impl CommittorService {
    pub fn new(
        authority: Keypair,
        config: CommittorConfig,
        chain_slot: Arc<AtomicU64>,
        engine: Engine,
        chainlink: Arc<ProdChainlink>,
        er_rpc: Arc<RpcClient>,
        callbacks: Arc<dyn ActionsCallbackScheduler>,
    ) -> Self {
        let rpc = MagicblockRpcClient::new_with_chain_slot_and_websocket(
            Arc::new(RpcClient::new_with_commitment(
                config.rpc_uri,
                config.commitment,
            )),
            chain_slot,
            config.websocket_uri,
        );
        let tables = TableMania::new(rpc.clone(), &authority, None);
        let task_info_provider = Arc::new(CachedTaskInfoProvider::new(RpcTaskInfoProvider::new(
            rpc.clone(),
        )));
        let outbox = Arc::new(InternalOutboxClient::new(engine.clone(), er_rpc));
        let backlog = Arc::new(Mutex::new(AccountsDbIntentBacklog::new(engine)));
        let (sender, receiver) = mpsc::channel(1000);
        let tasks = TaskTracker::new();
        let context = Arc::new(ExecutionContext {
            authority,
            intent_client: IntentExecutionClient::new(
                rpc.clone(),
                config.compute_budget_config.compute_unit_price,
            ),
            transaction_preparator: Arc::new(Preparator::new(
                rpc,
                tables.clone(),
                config.compute_budget_config,
            )),
            task_info_provider: task_info_provider.clone(),
            outbox_client: outbox.clone(),
            actions_callback_executor: callbacks,
            actions_timeout: config.actions_timeout,
            maintenance: tasks.clone(),
            cancel: CancellationToken::new(),
        });
        let executor = IntentExecutor::new(
            IntentStream::new(backlog.clone(), receiver),
            context.clone(),
            tasks,
        );
        let intake = IntentIntake {
            outbox_client: outbox.clone(),
            queue: Arc::new(IntentQueue {
                chainlink,
                outbox_client: outbox,
                task_info_provider,
                backlog,
                intent_sender: sender,
            }),
            slot_interval: config.slot_interval,
        };
        Self {
            intake,
            executor,
            context,
            tables,
        }
    }

    pub fn commit_id_provider(&self) -> CommitIdProvider {
        CommitIdProvider(self.context.task_info_provider.clone())
    }

    pub async fn run(self, mut shutdown: ShutdownHandle) {
        let stop = shutdown.child();
        let _cancel_on_drop = self.context.cancel.clone().drop_guard();
        let _stop_on_drop = stop.clone().drop_guard();
        let mut gc =
            tokio_util::task::AbortOnDropHandle::new(self.tables.spawn_garbage_collector(
                &self.context.authority,
                GarbageCollectorConfig::default(),
            ));
        let mut workers = JoinSet::new();
        let executor_stop = stop.clone();
        workers.spawn(async move {
            self.executor.run(executor_stop).await.map_err(CommittorServiceError::from)
        });
        let intake_stop = stop.clone();
        workers.spawn(async move {
            tokio::select! {
                biased;
                _ = intake_stop.cancelled() => Ok(()),
                result = async {
                    self.intake.queue.reschedule_intents().await?;
                    self.intake.run().await;
                    Ok(())
                } => result,
            }
        });
        let mut reason = tokio::select! {
            biased;
            _ = stop.cancelled() => ShutdownReason::Signalled,
            result = workers.join_next() => worker_reason(result),
            result = &mut gc => worker_reason(Some(result.map(|_| Ok(())))),
        };
        stop.cancel();
        reason = reason.combine(
            drain_workers(
                &mut workers,
                &self.context.maintenance,
                &self.context.cancel,
            )
            .await,
        );
        if !gc.is_finished() {
            gc.abort();
            let _ = gc.await;
        }
        shutdown.terminate(reason);
    }
}

async fn drain_workers(
    workers: &mut JoinSet<Result<(), CommittorServiceError>>,
    tasks: &TaskTracker,
    cancel: &CancellationToken,
) -> ShutdownReason {
    let mut reason = ShutdownReason::Signalled;
    let drain = async {
        while let Some(result) = workers.join_next().await {
            if !matches!(result, Ok(Ok(()))) {
                reason = std::mem::take(&mut reason).combine(worker_reason(Some(result)));
            }
        }
        tasks.close();
        tasks.wait().await;
    };
    if tokio::time::timeout(SHUTDOWN_GRACE, drain).await.is_err() {
        cancel.cancel();
        workers.shutdown().await;
        tasks.close();
        tasks.wait().await;
    }
    reason
}

fn worker_reason(
    result: Option<Result<Result<(), CommittorServiceError>, tokio::task::JoinError>>,
) -> ShutdownReason {
    match result {
        Some(Ok(Err(err))) => ShutdownReason::Error(Box::new(err)),
        Some(Err(err)) => ShutdownReason::Error(Box::new(err)),
        _ => ShutdownReason::Unexpected,
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum CommittorServiceError {
    #[error(transparent)]
    Outbox(#[from] OutboxIntentBundlesReaderError),
    #[error(transparent)]
    Worker(#[from] tokio::task::JoinError),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn shutdown_drains_then_cancels_remaining_work() {
        for stalled in [false, true] {
            let tasks = TaskTracker::new();
            let cancel = CancellationToken::new();
            let mut workers = JoinSet::new();
            let (finished, result) = tokio::sync::oneshot::channel();
            workers.spawn(tasks.track_future(async move {
                tokio::time::sleep(Duration::from_millis(50)).await;
                finished.send(()).unwrap();
                Ok(())
            }));
            if stalled {
                workers.spawn(tasks.track_future(std::future::pending()));
                let token = cancel.clone();
                tasks.spawn(async move { token.cancelled().await });
            }
            let start = tokio::time::Instant::now();
            let reason = drain_workers(&mut workers, &tasks, &cancel).await;
            assert!(matches!(reason, ShutdownReason::Signalled));
            assert!(
                result.await.is_ok(),
                "active work can finish during the drain"
            );
            assert!(workers.is_empty() && tasks.is_empty());
            assert_eq!(cancel.is_cancelled(), stalled);
            assert_eq!(
                start.elapsed(),
                if stalled { SHUTDOWN_GRACE } else { Duration::from_millis(50) }
            );
        }
    }
}
