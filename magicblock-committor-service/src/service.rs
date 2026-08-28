pub mod intent_client;

use std::{
    collections::{HashMap, HashSet},
    future::Future,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use futures_util::future::join_all;
use intent_client::{
    ERIntentClient, InternalIntentClientError, ScheduledBaseIntentMeta,
};
use magicblock_chainlink::{ProdChainlink, errors::ChainlinkResult};
use magicblock_metrics::metrics::{
    self, AccountFetchContext, AccountFetchReason,
};
use magicblock_program::{
    Pubkey, magic_scheduled_base_intent::ScheduledIntentBundle,
};
use nucleus::shutdown::{ShutdownHandle, ShutdownReason};
use tokio::{
    sync::{Notify, broadcast},
    task,
};
use tokio_util::sync::CancellationToken;
use tracing::{error, info, instrument};

use crate::{
    committor_processor::CommittorProcessor, error::CommittorServiceResult,
    intent_execution_manager::BroadcastedIntentExecutionResult,
    intent_executor::task_info_fetcher::TaskInfoFetcherResult,
    persist::RecoveredIntent,
};

const POISONED_MUTEX_MSG: &str =
    "IntentExecutionService intents_meta_map mutex poisoned";

pub type ChainlinkImpl = ProdChainlink;

pub struct IntentExecutionService<R> {
    /// Chainlink for notifying of undelegations
    chainlink: Arc<ChainlinkImpl>,
    /// ER client specific for Intent needs. Could be switched to RpcClient
    intent_rpc_client: Arc<R>,
    /// Processor of accepted intents
    processor: Arc<CommittorProcessor>,
    /// Time interval to scrape MagicContext(ER slot interval)
    // TODO(edwin): can be removed if LatestBlocK moved into magicblock-core
    slot_interval: Duration,
    /// Accepted intents whose execution result has not been handled yet.
    intents_meta_map: Arc<Mutex<HashMap<u64, ScheduledBaseIntentMeta>>>,
    /// Result notifications currently being submitted back to the Engine.
    processing_results: Arc<AtomicUsize>,
    intents_changed: Arc<Notify>,
}

impl<R> IntentExecutionService<R>
where
    R: ERIntentClient,
    R::Error: Into<IntentExecutionServiceError>,
{
    pub fn new(
        chainlink: Arc<ChainlinkImpl>,
        intent_rpc_client: R,
        processor: Arc<CommittorProcessor>,
        slot_interval: Duration,
    ) -> Self {
        Self {
            chainlink,
            intent_rpc_client: Arc::new(intent_rpc_client),
            processor,
            slot_interval,
            intents_meta_map: Arc::new(Mutex::default()),
            processing_results: Arc::new(AtomicUsize::new(0)),
            intents_changed: Arc::new(Notify::new()),
        }
    }

    /// Runs both intent workers and reports after they have stopped.
    pub async fn run(self, mut shutdown: ShutdownHandle) {
        let result = self.run_workers(shutdown.child()).await;
        let reason = match result {
            Err(error) => ShutdownReason::Error(Box::new(error)),
            Ok(()) if shutdown.requested() => ShutdownReason::Signalled,
            Ok(()) => ShutdownReason::Unexpected,
        };
        shutdown.terminate(reason);
    }

    async fn run_workers(
        self,
        cancellation_token: CancellationToken,
    ) -> Result<(), IntentExecutionServiceError> {
        let result_subscriber = self.processor.subscribe_for_results();
        let result_token = CancellationToken::new();
        let result_shutdown = result_token.clone();
        let intents_meta_map = self.intents_meta_map.clone();
        let processing_results = self.processing_results.clone();
        let intents_changed = self.intents_changed.clone();
        let intent_rpc_client = self.intent_rpc_client.clone();
        let result_worker = Self::result_processor(
            result_subscriber,
            result_shutdown,
            intents_meta_map,
            processing_results,
            intents_changed,
            intent_rpc_client,
        );
        tokio::pin!(result_worker);
        let accept_token = CancellationToken::new();
        let accept_shutdown = accept_token.clone();
        let pending_intents = self.intents_meta_map.clone();
        let processing_results = self.processing_results.clone();
        let pending_changed = self.intents_changed.clone();
        let accept_worker = self.accept_worker(accept_shutdown);
        tokio::pin!(accept_worker);

        tokio::select! {
            biased;
            _ = cancellation_token.cancelled() => {
                // Do not drop completion notifications for already accepted
                // intents: stop ingress, drain results, then stop observation.
                accept_token.cancel();
                accept_worker.await;

                tokio::select! {
                    biased;
                    _ = Self::wait_for_intents(
                        pending_intents,
                        processing_results,
                        pending_changed,
                    ) => {}
                    _ = &mut result_worker => {
                        return Err(IntentExecutionServiceError::WorkerStopped(
                            "result processor",
                        ));
                    }
                }

                result_token.cancel();
                result_worker.await;
                Ok(())
            }
            _ = &mut accept_worker => {
                result_token.cancel();
                result_worker.await;
                Err(IntentExecutionServiceError::WorkerStopped(
                    "intent acceptor",
                ))
            }
            _ = &mut result_worker => {
                accept_token.cancel();
                accept_worker.await;
                Err(IntentExecutionServiceError::WorkerStopped(
                    "result processor",
                ))
            }
        }
    }

    async fn wait_for_intents(
        intents_meta_map: Arc<Mutex<HashMap<u64, ScheduledBaseIntentMeta>>>,
        processing_results: Arc<AtomicUsize>,
        intents_changed: Arc<Notify>,
    ) {
        loop {
            let intents_empty = intents_meta_map
                .lock()
                .expect(POISONED_MUTEX_MSG)
                .is_empty();
            if intents_empty && processing_results.load(Ordering::SeqCst) == 0 {
                return;
            }
            intents_changed.notified().await;
        }
    }

    async fn accept_worker(self, cancellation_token: CancellationToken) {
        if let Err(err) = self.reschedule_pending_bundles().await {
            error!(error = ?err, "Failed to reschedule pending bundles")
        }

        let mut interval = tokio::time::interval(self.slot_interval);
        loop {
            tokio::select! {
                biased;
                _ = cancellation_token.cancelled() => {
                    break;
                }
                _ = interval.tick() => {
                    let accept_result = self
                        .intent_rpc_client
                        .accept_scheduled_intents()
                        .await;
                    let intent_bundles = match accept_result {
                        Ok(value) => value,
                        Err(err) => {
                            error!("Failed to accept intents: {}", err);
                            continue;
                        }
                    };

                    if let Err(err) = self.schedule_intent_execution(intent_bundles).await {
                        error!("Failed to schedule intent execution: {}", err);
                    }
                }
            }
        }
    }

    async fn reschedule_pending_bundles(&self) -> CommittorServiceResult<()> {
        let (slot, pending, failed) = {
            let (slot, pending, failed) = tokio::join!(
                self.processor.get_slot(),
                self.processor.load_pending_intent_bundles(),
                self.processor.load_recovery_intent_bundles(),
            );
            let slot = slot?;
            let pending = pending.inspect_err(|err| {
                error!(error = ?err, "Failed to load pending intent bundles for recovery");
            })?;
            let failed = failed.inspect_err(|err| {
                error!(error = ?err, "Failed to load failed intent bundles for recovery");
            })?;
            (slot, pending, failed)
        };
        if pending.is_empty() && failed.is_empty() {
            return Ok(());
        }

        let mut bundles = self.retain_recoverable_intents(failed, slot).await;
        bundles.extend(pending);
        bundles.sort_by_key(|bundle| bundle.id);
        if bundles.is_empty() {
            return Ok(());
        }

        self.processor
            .refresh_intent_bundles(&mut bundles, slot)
            .await?;

        // Schedule  without initial persisitance as bundle already exists in db
        self.process_intent_bundles(bundles, |bundles| {
            self.processor.schedule_recovered_intent_bundles(bundles)
        })
        .await
    }

    async fn schedule_intent_execution(
        &self,
        intent_bundles: Vec<ScheduledIntentBundle>,
    ) -> CommittorServiceResult<()> {
        if intent_bundles.is_empty() {
            return Ok(());
        }

        metrics::inc_committor_intents_count_by(intent_bundles.len() as u64);

        self.process_intent_bundles(intent_bundles, |bundles| {
            self.processor.schedule_intent_bundles(bundles)
        })
        .await
    }

    async fn process_intent_bundles<F, Fut>(
        &self,
        intent_bundles: Vec<ScheduledIntentBundle>,
        schedule: F,
    ) -> CommittorServiceResult<()>
    where
        F: FnOnce(Vec<ScheduledIntentBundle>) -> Fut,
        Fut: Future<Output = CommittorServiceResult<()>>,
    {
        if intent_bundles.is_empty() {
            return Ok(());
        }

        // Add metas for intent we schedule
        let intent_ids: Vec<u64>;
        let pubkeys_being_undelegated = {
            let mut intent_metas =
                self.intents_meta_map.lock().expect(POISONED_MUTEX_MSG);
            let mut pubkeys_being_undelegated = HashSet::<Pubkey>::new();
            intent_bundles.iter().for_each(|intent| {
                intent_metas
                    .insert(intent.id, ScheduledBaseIntentMeta::new(intent));
                if let Some(undelegate) = intent.get_undelegate_intent_pubkeys()
                {
                    pubkeys_being_undelegated.extend(undelegate);
                }
            });
            intent_ids = intent_bundles.iter().map(|b| b.id).collect();
            pubkeys_being_undelegated.into_iter().collect::<Vec<_>>()
        };

        self.process_undelegation_requests(pubkeys_being_undelegated)
            .await;

        let result = schedule(intent_bundles).await;
        // If scheduling failed remove from map
        if result.is_err() {
            let mut intent_metas =
                self.intents_meta_map.lock().expect(POISONED_MUTEX_MSG);
            intent_ids.iter().for_each(|id| {
                intent_metas.remove(id);
            });
            self.intents_changed.notify_one();
        }
        result
    }

    async fn process_undelegation_requests(&self, pubkeys: Vec<Pubkey>) {
        let mut join_set = task::JoinSet::new();
        for pubkey in pubkeys.into_iter() {
            let chainlink = self.chainlink.clone();
            join_set.spawn(async move {
                (pubkey, chainlink.undelegation_requested(pubkey).await)
            });
        }
        let sub_errors = join_set
            .join_all()
            .await
            .into_iter()
            .filter_map(|(pubkey, inner_result)| {
                if let Err(err) = inner_result {
                    Some(format!(
                        "Subscribing to account {} failed: {}",
                        pubkey, err
                    ))
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        if !sub_errors.is_empty() {
            // Instead of aborting the entire commit we log an error here, however
            // this means that the undelegated accounts stay in a problematic state
            // in the validator and are not synced from chain.
            // We could implement a retry mechanism inside of chainlink in the future.
            error!(
                error_count = sub_errors.len(),
                "Failed to subscribe to accounts being undelegated"
            );
        }
    }

    #[instrument(skip(
        result_subscription,
        cancellation_token,
        intents_meta_map,
        processing_results,
        intents_changed,
        intent_client
    ))]
    async fn result_processor(
        mut result_subscription: broadcast::Receiver<
            BroadcastedIntentExecutionResult,
        >,
        cancellation_token: CancellationToken,
        intents_meta_map: Arc<Mutex<HashMap<u64, ScheduledBaseIntentMeta>>>,
        processing_results: Arc<AtomicUsize>,
        intents_changed: Arc<Notify>,
        intent_client: Arc<R>,
    ) {
        loop {
            let execution_result = tokio::select! {
                biased;
                _ = cancellation_token.cancelled() => {
                    info!("Shutting down");
                    return;
                }
                execution_result = result_subscription.recv() => {
                    match execution_result {
                        Ok(result) => result,
                        Err(broadcast::error::RecvError::Closed) => {
                            info!("Intent execution service shut down");
                            break;
                        }
                        Err(broadcast::error::RecvError::Lagged(skipped)) => {
                            // SAFETY: This shouldn't happen as our tx execution is faster than Intent execution on Base layer
                            // If this ever happens it requires investigation
                            error!(skipped_count = skipped, "Lagging behind intent execution");
                            continue;
                        }
                    }
                }
            };

            if let Err(err) = Self::process_execution_result(
                &intent_client,
                execution_result,
                &intents_meta_map,
                &processing_results,
                &intents_changed,
            )
            .await
            {
                error!(error = ?err, "Failed process intent execution results");
            }
        }
    }

    async fn process_execution_result(
        intent_client: &Arc<R>,
        execution_result: BroadcastedIntentExecutionResult,
        intents_meta_map: &Arc<Mutex<HashMap<u64, ScheduledBaseIntentMeta>>>,
        processing_results: &AtomicUsize,
        intents_changed: &Notify,
    ) -> Result<(), R::Error> {
        let intent_id = execution_result.id;
        let intent_meta = {
            let Some(intent_meta) = intents_meta_map
                .lock()
                .expect(POISONED_MUTEX_MSG)
                .remove(&intent_id)
            else {
                // Possible if we have duplicate Intents
                // First one will remove id from map and second could fail.
                // This should not happen and needs investigation!
                error!(intent_id, "Failed to find intent metadata");
                return Ok(());
            };
            processing_results.fetch_add(1, Ordering::SeqCst);
            intent_meta
        };

        let result = intent_client
            .notify_commit_sent(intent_meta, execution_result)
            .await;
        processing_results.fetch_sub(1, Ordering::SeqCst);
        intents_changed.notify_one();

        result
    }

    async fn retain_recoverable_intents(
        &self,
        recovered: Vec<RecoveredIntent>,
        min_context_slot: u64,
    ) -> Vec<ScheduledIntentBundle> {
        const JOIN_CHUNK_SIZE: usize = 10;

        let mut keep = Vec::with_capacity(recovered.len());
        for chunk in recovered.chunks(JOIN_CHUNK_SIZE) {
            keep.extend(
                join_all(chunk.iter().map(|recovered| async move {
                    self.is_intent_retriable(recovered, min_context_slot).await
                }))
                .await,
            );
        }

        recovered
            .into_iter()
            .enumerate()
            .filter(|(index, _)| keep[*index])
            .map(|(_, recovered)| recovered.bundle)
            .collect()
    }

    async fn is_intent_retriable(
        &self,
        recovered: &RecoveredIntent,
        min_context_slot: u64,
    ) -> bool {
        let pubkeys = recovered.bundle.get_all_committed_pubkeys();
        if !self
            .is_same_delegation_session(&pubkeys, recovered)
            .await
            .inspect_err(|err| {
                error!(intent_id = recovered.bundle.id, error = ?err, "Failed to check delegation session for recovery");
            })
            .unwrap_or(false)
        {
            return false;
        }

        self.is_valid_nonce(&pubkeys, recovered, min_context_slot)
            .await
            .inspect_err(|err| {
                error!(intent_id = recovered.bundle.id, error = ?err, "Failed to check commit nonce for recovery");
            })
            .unwrap_or(false)
    }

    async fn is_same_delegation_session(
        &self,
        pubkeys: &[Pubkey],
        recovered: &RecoveredIntent,
    ) -> ChainlinkResult<bool> {
        let recovered_accounts = recovered.bundle.get_all_committed_accounts();
        let current_sessions = self
            .chainlink
            .account_delegation_sessions(
                pubkeys,
                AccountFetchContext::internal(
                    AccountFetchReason::RequestedAccount,
                ),
            )
            .await?
            .into_iter()
            .collect::<Option<Vec<_>>>();
        let Some(current_sessions) = current_sessions else {
            return Ok(false);
        };
        if current_sessions.len() != pubkeys.len() {
            return Ok(false);
        }

        Ok(recovered_accounts.into_iter().zip(current_sessions).all(
            |(recovered, current)| {
                current.locally_protected
                    && recovered.remote_slot == current.remote_slot
            },
        ))
    }

    async fn is_valid_nonce(
        &self,
        pubkeys: &[Pubkey],
        recovered: &RecoveredIntent,
        min_context_slot: u64,
    ) -> TaskInfoFetcherResult<bool> {
        let current_nonces = self
            .processor
            .fetch_current_commit_nonces(pubkeys, min_context_slot)
            .await?;
        Ok(recovered.commit_ids.iter().all(|(pubkey, commit_id)| {
            current_nonces
                .get(pubkey)
                .is_some_and(|current_nonce| commit_id >= current_nonce)
        }))
    }
}

#[derive(thiserror::Error, Debug)]
pub enum IntentExecutionServiceError {
    #[error("Intent execution worker '{0}' stopped unexpectedly")]
    WorkerStopped(&'static str),
    #[error("IntentRpcClientError: {0}")]
    IntentRpcClientError(#[from] InternalIntentClientError),
}
