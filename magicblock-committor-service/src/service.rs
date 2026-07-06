pub mod intent_client;

use std::{
    collections::{HashMap, HashSet},
    future::Future,
    mem,
    sync::{Arc, Mutex},
    time::Duration,
};

use futures_util::future::join_all;
use intent_client::{
    ERIntentClient, InternalIntentClientError, ScheduledBaseIntentMeta,
};
use magicblock_chainlink::{ProdChainlink, ProdInnerChainlink};
use magicblock_metrics::metrics::{
    self, AccountFetchContext, AccountFetchReason,
};
use magicblock_program::{
    Pubkey, magic_scheduled_base_intent::ScheduledIntentBundle,
};
use tokio::{
    sync::broadcast,
    task,
    task::{JoinError, JoinHandle},
};
use tokio_util::sync::CancellationToken;
use tracing::{error, info, instrument, warn};

use crate::{
    committor_processor::CommittorProcessor, error::CommittorServiceResult,
    intent_execution_manager::BroadcastedIntentExecutionResult,
};

const POISONED_MUTEX_MSG: &str = "ServiceInner intents_meta_map mutex poisoned";

pub type InnerChainlinkImpl = ProdInnerChainlink;
pub type ChainlinkImpl = ProdChainlink;

pub enum IntentExecutionService<R> {
    Created(ServiceInner<R>),
    Started(JoinHandle<()>),
    Stopped,
    Error,
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
        cancellation_token: CancellationToken,
    ) -> Self {
        Self::Created(ServiceInner::new(
            chainlink,
            intent_rpc_client,
            processor,
            slot_interval,
            cancellation_token,
        ))
    }

    fn take(&mut self) -> Self {
        mem::replace(self, Self::Error)
    }

    pub fn start(&mut self) -> Result<(), IntentExecutionServiceError> {
        let Self::Created(service) = self.take() else {
            return Err(IntentExecutionServiceError::InvalidState(
                "service must be in Created state to start".into(),
            ));
        };

        let handle = service.start();
        *self = Self::Started(handle);
        Ok(())
    }

    pub async fn stop(&mut self) -> Result<(), IntentExecutionServiceError> {
        let Self::Started(handle) = self.take() else {
            return Err(IntentExecutionServiceError::InvalidState(
                "service must be in Started state to stop".into(),
            ));
        };

        handle.await?;
        *self = Self::Stopped;
        Ok(())
    }
}

pub struct ServiceInner<R> {
    /// Chainlink for notifying of undelegations
    chainlink: Arc<ChainlinkImpl>,
    /// ER client specific for Intent needs. Could be switched to RpcClient
    intent_rpc_client: Arc<R>,
    /// Processor of accepted intents
    processor: Arc<CommittorProcessor>,
    /// Time interval to scrape MagicContext(ER slot interval)
    // TODO(edwin): can be removed if LatestBlocK moved into magicblock-core
    slot_interval: Duration,
    cancellation_token: CancellationToken,
    /// Meta for ongoing executing intents
    intents_meta_map: Arc<Mutex<HashMap<u64, ScheduledBaseIntentMeta>>>,
}

impl<R> ServiceInner<R>
where
    R: ERIntentClient,
    R::Error: Into<IntentExecutionServiceError>,
{
    pub fn new(
        chainlink: Arc<ChainlinkImpl>,
        intent_rpc_client: R,
        processor: Arc<CommittorProcessor>,
        slot_interval: Duration,
        cancellation_token: CancellationToken,
    ) -> Self {
        Self {
            chainlink,
            intent_rpc_client: Arc::new(intent_rpc_client),
            processor,
            slot_interval,
            cancellation_token,
            intents_meta_map: Arc::new(Mutex::default()),
        }
    }

    /// Starts 2 workers: one accepting intents, another awaiting and handling results
    fn start(self) -> JoinHandle<()> {
        let result_subscriber = self.processor.subscribe_for_results();
        let cancellation_token = self.cancellation_token.clone();
        tokio::spawn(Self::result_processor(
            result_subscriber,
            cancellation_token,
            self.intents_meta_map.clone(),
            self.intent_rpc_client.clone(),
        ));

        tokio::task::spawn(self.accept_worker())
    }

    async fn accept_worker(self) {
        if let Err(err) = self.reschedule_pending_bundles().await {
            error!(error = ?err, "Failed to reschedule pending bundles")
        }

        let mut interval = tokio::time::interval(self.slot_interval);
        loop {
            tokio::select! {
                biased;
                _ = self.cancellation_token.cancelled() => {
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
        // Fetch pending bundles from DB
        let mut bundles =
            self.processor.pending_intent_bundles().await.inspect_err(|err| {
                error!(error = ?err, "Failed to load pending intent bundles for recovery");
            })?;
        if bundles.is_empty() {
            return Ok(());
        }

        // Retain only recoverable bundles
        self.retain_recoverable_intent_bundles(&mut bundles).await;

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
        intent_client
    ))]
    async fn result_processor(
        mut result_subscription: broadcast::Receiver<
            BroadcastedIntentExecutionResult,
        >,
        cancellation_token: CancellationToken,
        intents_meta_map: Arc<Mutex<HashMap<u64, ScheduledBaseIntentMeta>>>,
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

            if let Err(err) = ServiceInner::<R>::process_execution_result(
                &intent_client,
                execution_result,
                &intents_meta_map,
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
    ) -> Result<(), R::Error> {
        let intent_id = execution_result.id;
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

        intent_client
            .notify_commit_sent(intent_meta, execution_result)
            .await?;

        Ok(())
    }

    /// Retains bundles whose accounts are still delegated
    async fn retain_recoverable_intent_bundles(
        &self,
        bundles: &mut Vec<ScheduledIntentBundle>,
    ) {
        let results = join_all(
            bundles.iter().map(|b| b.get_all_committed_pubkeys()).map(
                |pubkeys| async move {
                    #[allow(deprecated)]
                    let result = self
                        .chainlink
                        .accounts_delegated_on_base_and_er(
                            &pubkeys,
                            AccountFetchContext::internal(
                                AccountFetchReason::RequestedAccount,
                            ),
                        )
                        .await;
                    result
                },
            ),
        )
        .await;

        let mut results_iter = results.into_iter();
        bundles.retain(|bundle| {
            let Some(result) = results_iter.next() else {
                error!("Results and bundles must have equal length");
                return false;
            };
            match result {
                Ok(delegated) if delegated.iter().all(|d| *d) => true,
                Ok(_) => {
                    warn!(
                        intent_id = bundle.id,
                        "Skipping recovered commit intent because not all accounts are delegated on base and ER"
                    );
                    false
                }
                Err(err) => {
                    error!(
                        intent_id = bundle.id,
                        error = ?err,
                        "Failed to verify recovered commit intent accounts"
                    );
                    false
                }
            }
        });
    }
}

#[derive(thiserror::Error, Debug)]
pub enum IntentExecutionServiceError {
    #[error("Invalid state: {0}")]
    InvalidState(String),
    #[error("JoinError: {0}")]
    JoinError(#[from] JoinError),
    #[error("IntentRpcClientError: {0}")]
    IntentRpcClientError(#[from] InternalIntentClientError),
}
