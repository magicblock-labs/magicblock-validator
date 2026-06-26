use std::{
    collections::{hash_map::Entry, HashMap},
    sync::{atomic::AtomicU64, Arc, Mutex},
};

use futures_util::future::join_all;
use magicblock_core::traits::ActionsCallbackScheduler;
use magicblock_program::outbox_intent_bundles::OutboxIntentBundle;
use magicblock_rpc_client::MagicblockRpcClient;
use magicblock_table_mania::{GarbageCollectorConfig, TableMania};
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use tokio::sync::{broadcast, oneshot, oneshot::error::RecvError};
use tracing::{error, info, instrument};

use crate::{
    config::ChainConfig,
    error::{CommittorServiceError, CommittorServiceResult},
    intent_engine::{
        db::DummyDB, BroadcastedIntentExecutionResult, IntentEngineHandle,
    },
    intent_executor::{
        error::IntentExecutorError,
        intent_executor_factory::ExecutorConfig,
        task_info_fetcher::{
            CacheTaskInfoFetcher, RpcTaskInfoFetcher, TaskInfoFetcher,
            TaskInfoFetcherResult,
        },
    },
    outbox_client::OutboxClient,
};

const POISONED_MUTEX_MSG: &str =
    "CommittorProcessor pending messages mutex poisoned!";

type BundleResultListener = oneshot::Sender<BroadcastedIntentExecutionResult>;

pub struct CommittorProcessor {
    authority: Keypair,
    _table_mania: TableMania,
    magic_rpc_client: MagicblockRpcClient,
    commits_scheduler: IntentEngineHandle<DummyDB>,
    task_info_fetcher: Arc<CacheTaskInfoFetcher<RpcTaskInfoFetcher>>,
    pending_result_listeners: Arc<Mutex<HashMap<u64, BundleResultListener>>>,
}

impl CommittorProcessor {
    pub fn new<A, O>(
        authority: Keypair,
        chain_config: ChainConfig,
        chain_slot: Option<Arc<AtomicU64>>,
        outbox_client: Arc<O>,
        actions_callback_executor: A,
    ) -> Self
    where
        A: ActionsCallbackScheduler,
        O: OutboxClient,
        O::Error: Into<IntentExecutorError>,
    {
        let rpc_client = RpcClient::new_with_commitment(
            chain_config.rpc_uri.to_string(),
            chain_config.commitment,
        );
        let rpc_client = Arc::new(rpc_client);
        let websocket_uri = chain_config.websocket_uri.clone();
        let magic_block_rpc_client = match (chain_slot, websocket_uri) {
            (Some(chain_slot), websocket_uri) => {
                MagicblockRpcClient::new_with_chain_slot_and_websocket(
                    rpc_client,
                    chain_slot,
                    websocket_uri,
                )
            }
            (None, Some(websocket_uri)) => {
                MagicblockRpcClient::new_with_websocket(
                    rpc_client,
                    Some(websocket_uri),
                )
            }
            (None, None) => MagicblockRpcClient::new(rpc_client),
        };

        // Create TableMania
        let gc_config = GarbageCollectorConfig::default();
        let table_mania = TableMania::new(
            magic_block_rpc_client.clone(),
            &authority,
            Some(gc_config),
        );

        // Create commit scheduler
        let task_info_fetcher = Arc::new(CacheTaskInfoFetcher::new(
            RpcTaskInfoFetcher::new(magic_block_rpc_client.clone()),
        ));
        let commits_scheduler = IntentEngineHandle::new(
            magic_block_rpc_client.clone(),
            // TODO(edwin): use DumberDb
            DummyDB::new(),
            task_info_fetcher.clone(),
            outbox_client,
            table_mania.clone(),
            ExecutorConfig {
                compute_budget_config: chain_config
                    .compute_budget_config
                    .clone(),
                actions_timeout: chain_config.actions_timeout,
            },
            actions_callback_executor,
        );

        let result_subscription = commits_scheduler.subscribe_for_results();
        let pending_result_listeners = Arc::new(Mutex::new(HashMap::new()));
        tokio::spawn(Self::dispatcher(
            result_subscription,
            pending_result_listeners.clone(),
        ));

        Self {
            authority,
            _table_mania: table_mania,
            magic_rpc_client: magic_block_rpc_client,
            commits_scheduler,
            task_info_fetcher,
            pending_result_listeners,
        }
    }

    #[instrument(skip(self, intent_bundles))]
    pub async fn schedule_intent_bundles(
        &self,
        intent_bundles: Vec<OutboxIntentBundle>,
    ) -> CommittorServiceResult<()> {
        self.commits_scheduler
            .schedule(intent_bundles)
            .await
            .inspect_err(|err| {
                error!(error = ?err, "Failed to schedule intent");
            })?;

        Ok(())
    }

    pub async fn execute_intent_bundles(
        &self,
        intent_bundles: Vec<OutboxIntentBundle>,
    ) -> CommittorServiceResult<Vec<BroadcastedIntentExecutionResult>> {
        // Critical section
        let (receivers, inserted_ids) = {
            let mut result_listeners = self
                .pending_result_listeners
                .lock()
                .expect(POISONED_MUTEX_MSG);

            let mut receivers = Vec::with_capacity(intent_bundles.len());
            let mut inserted_ids = Vec::with_capacity(intent_bundles.len());

            for intent in &intent_bundles {
                let (sender, receiver) = oneshot::channel();
                match result_listeners.entry(intent.id) {
                    Entry::Vacant(vacant) => {
                        vacant.insert(sender);
                        inserted_ids.push(intent.id);
                        receivers.push(receiver);
                    }
                    Entry::Occupied(_) => {
                        for id in &inserted_ids {
                            result_listeners.remove(id);
                        }
                        return Err(
                            CommittorServiceError::RepeatingMessageError(
                                intent.id,
                            ),
                        );
                    }
                }
            }
            (receivers, inserted_ids)
        };

        if let Err(err) = self.schedule_intent_bundles(intent_bundles).await {
            let mut result_listeners = self
                .pending_result_listeners
                .lock()
                .expect(POISONED_MUTEX_MSG);
            for id in &inserted_ids {
                result_listeners.remove(id);
            }
            return Err(err);
        }

        let results = join_all(receivers.into_iter())
            .await
            .into_iter()
            .collect::<Result<Vec<_>, RecvError>>()?;

        Ok(results)
    }

    /// Creates a subscription for results of BaseIntent execution
    pub fn subscribe_for_results(
        &self,
    ) -> broadcast::Receiver<BroadcastedIntentExecutionResult> {
        self.commits_scheduler.subscribe_for_results()
    }

    /// Fetches current commit nonces
    pub async fn fetch_current_commit_nonces(
        &self,
        pubkeys: &[Pubkey],
        min_context_slot: u64,
    ) -> TaskInfoFetcherResult<HashMap<Pubkey, u64>> {
        self.task_info_fetcher
            .fetch_current_commit_nonces(pubkeys, min_context_slot)
            .await
    }

    /// Dispatch worker
    #[instrument(skip(pending_result_listeners, results_subscription))]
    async fn dispatcher(
        mut results_subscription: broadcast::Receiver<
            BroadcastedIntentExecutionResult,
        >,
        pending_result_listeners: Arc<
            Mutex<HashMap<u64, BundleResultListener>>,
        >,
    ) {
        loop {
            let execution_result = match results_subscription.recv().await {
                Ok(result) => result,
                Err(broadcast::error::RecvError::Closed) => {
                    info!("Intent execution shutdown");
                    break;
                }
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    // SAFETY: not really feasible to happen as this function is way faster than Intent execution
                    // requires investigation if ever happens!
                    error!(skipped, "Dispatcher lag detected");
                    continue;
                }
            };

            let sender = if let Some(sender) = pending_result_listeners
                .lock()
                .expect(POISONED_MUTEX_MSG)
                .remove(&execution_result.id)
            {
                sender
            } else {
                continue;
            };

            if let Err(execution_result) = sender.send(execution_result) {
                error!(
                    intent_id = execution_result.id,
                    "Failed to send execution result"
                );
            }
        }
    }
}
