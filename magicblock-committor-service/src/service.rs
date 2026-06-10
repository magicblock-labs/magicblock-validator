pub mod acceptor;
pub mod outbox_intent_bundles_reader;

use std::{
    collections::{HashMap, HashSet},
    future::Future,
    mem,
    num::NonZeroUsize,
    sync::{Arc, Mutex},
    time::Duration,
};

use futures_util::future::join_all;
use magicblock_account_cloner::ChainlinkCloner;
use magicblock_chainlink::{ProdChainlink, ProdInnerChainlink};
use magicblock_metrics::metrics::{self, AccountFetchOrigin};
use magicblock_program::{
    magic_scheduled_base_intent::ScheduledIntentBundle, Pubkey, SentCommit,
};
use solana_hash::Hash;
use solana_transaction::Transaction;
use tokio::{
    sync::broadcast,
    task,
    task::{JoinError, JoinHandle},
};
use tokio_util::sync::CancellationToken;
use tracing::{error, info, instrument, warn};

use crate::{
    committor_processor::CommittorProcessor,
    error::CommittorServiceResult,
    intent_execution_manager::BroadcastedIntentExecutionResult,
    intent_executor::ExecutionOutput,
    outbox_client::{InternalOutboxClientError, OutboxClient},
    service::outbox_intent_bundles_reader::{
        InternalOutboxIntentBundlesReaderError, OutboxIntentBundlesReader,
    },
};

const POISONED_MUTEX_MSG: &str = "ServiceInner intents_meta_map mutex poisoned";

pub type InnerChainlinkImpl = ProdInnerChainlink<ChainlinkCloner>;
pub type ChainlinkImpl = ProdChainlink<ChainlinkCloner>;

pub enum IntentExecutionService<R> {
    Created(ServiceInner<R>),
    Started(JoinHandle<()>),
    Stopped,
    Error,
}

impl<R> IntentExecutionService<R>
where
    R: OutboxClient,
    // ERIntentClient errors should be convertible to Service errors
    R::Error: Into<IntentExecutionServiceError>,
    // OutboxReader errors should be convertible to Service errors
    <R::OutboxReader as OutboxIntentBundlesReader>::Error:
        Into<IntentExecutionServiceError>,
{
    pub fn new(
        chainlink: Arc<ChainlinkImpl>,
        intent_client: R,
        processor: Arc<CommittorProcessor>,
        slot_interval: Duration,
        cancellation_token: CancellationToken,
    ) -> Self {
        Self::Created(ServiceInner::new(
            chainlink,
            intent_client,
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

pub struct ServiceInner<O> {
    /// Chainlink for notifying of undelegations
    chainlink: Arc<ChainlinkImpl>,
    /// ER client specific for Intent needs. Could be switched to RpcClient
    outbox_client: Arc<O>,
    /// Processor of accepted intents
    processor: Arc<CommittorProcessor>,
    /// Time interval to scrape MagicContext(ER slot interval)
    // TODO(edwin): can be removed if LatestBlocK moved into magicblock-core
    slot_interval: Duration,
    cancellation_token: CancellationToken,
    /// Meta for ongoing executing intents
    intents_meta_map: Arc<Mutex<HashMap<u64, ScheduledBaseIntentMeta>>>,
}

impl<O> ServiceInner<O>
where
    O: OutboxClient,
    // ERIntentClient errors should be convertible to Service errors
    O::Error: Into<IntentExecutionServiceError>,
    // OutboxReader errors should be convertible to Service errors
    <O::OutboxReader as OutboxIntentBundlesReader>::Error:
        Into<IntentExecutionServiceError>,
{
    pub fn new(
        chainlink: Arc<ChainlinkImpl>,
        outbox_client: Arc<O>,
        processor: Arc<CommittorProcessor>,
        slot_interval: Duration,
        cancellation_token: CancellationToken,
    ) -> Self {
        Self {
            chainlink,
            outbox_client,
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
            self.outbox_client.clone(),
        ));

        tokio::task::spawn(self.accept_worker())
    }

    async fn accept_worker(self) {
        // Reschedule existing outbox intents first
        // We need to ensure that accounts in outbox a scheduled before
        // we accept new incoming Intents
        self.reschedule_intents()
            .await
            .inspect_err(|err| {
                error!(error = ?err, "Failed to reschedule pending bundles")
            })
            // TODO(edwin): early shutdown or cleanup errors to avoid this
            .expect("Failed to reschedule intents");

        // if let Err(err) = self.reschedule_pending_bundles().await {
        //     error!(error = ?err, "Failed to reschedule pending bundles")
        // }

        let mut interval = tokio::time::interval(self.slot_interval);
        loop {
            tokio::select! {
                biased;
                _ = self.cancellation_token.cancelled() => {
                    break;
                }
                _ = interval.tick() => {
                    let accept_result = self
                        .outbox_client
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

    async fn reschedule_intents(
        &self,
    ) -> Result<(), IntentExecutionServiceError> {
        /// Number of intents rescheduled at once
        const RESCHEDULE_CHUNK_SIZE: NonZeroUsize =
            NonZeroUsize::new(1000).unwrap();

        let mut outbox_bundles_reader = self.outbox_client.outbox_reader();
        loop {
            // Read by chunks in order not to overload `IntentExecutionEngine`
            let intent_bundles_chunk = outbox_bundles_reader
                .read(RESCHEDULE_CHUNK_SIZE)
                .await
                .map_err(Into::into)?;
            if intent_bundles_chunk.is_empty() {
                return Ok(());
            }

            // TODO(edwin): use status
            let read_len = intent_bundles_chunk.len();
            let intent_bundles = intent_bundles_chunk
                .into_iter()
                .map(|el| el.inner)
                .collect();

            // Schedule  without initial persistence as bundle already exists in db
            let result = self
                .process_intent_bundles(intent_bundles, |bundles| {
                    self.processor.schedule_recovered_intent_bundles(bundles)
                })
                .await;
            if let Err(err) = result {
                error!(error = ?err, "Failed to reschedule pending bundles")
            }

            // Check if we've rescheduled intents from Outbox
            if read_len != RESCHEDULE_CHUNK_SIZE.get() {
                return Ok(());
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
        outbox_client
    ))]
    async fn result_processor(
        mut result_subscription: broadcast::Receiver<
            BroadcastedIntentExecutionResult,
        >,
        cancellation_token: CancellationToken,
        intents_meta_map: Arc<Mutex<HashMap<u64, ScheduledBaseIntentMeta>>>,
        outbox_client: Arc<O>,
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

            if let Err(err) = ServiceInner::<O>::process_execution_result(
                &outbox_client,
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
        outbox_client: &Arc<O>,
        execution_result: BroadcastedIntentExecutionResult,
        intents_meta_map: &Arc<Mutex<HashMap<u64, ScheduledBaseIntentMeta>>>,
    ) -> Result<(), O::Error> {
        // Create IntentMeta
        let intent_id = execution_result.id;
        // Remove intent from metas
        let Some(mut intent_meta) = intents_meta_map
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

        let sent_transaction =
            mem::take(&mut intent_meta.intent_sent_transaction);
        let sent_commit = ServiceInner::<O>::build_sent_commit(
            intent_id,
            intent_meta,
            execution_result,
        );
        outbox_client
            .notify_commit_sent(sent_transaction, sent_commit)
            .await?;

        Ok(())
    }

    fn build_sent_commit(
        intent_id: u64,
        intent_meta: ScheduledBaseIntentMeta,
        result: BroadcastedIntentExecutionResult,
    ) -> SentCommit {
        let error_message =
            result.as_ref().err().map(|err| format!("{:?}", err));
        let chain_signatures = match result.inner {
            Ok(value) => match value {
                ExecutionOutput::SingleStage(signature) => vec![signature],
                ExecutionOutput::TwoStage {
                    commit_signature,
                    finalize_signature,
                } => vec![commit_signature, finalize_signature],
            },
            Err(err) => {
                error!(
                    "Failed to commit intent: {}, slot: {}, blockhash: {}. {:?}",
                    intent_id, intent_meta.slot, intent_meta.blockhash, err
                );
                err.signatures()
                    .map(|(commit, finalize)| {
                        finalize
                            .map(|finalize| vec![commit, finalize])
                            .unwrap_or(vec![commit])
                    })
                    .unwrap_or_default()
            }
        };
        let patched_errors = result
            .patched_errors
            .iter()
            .map(|err| {
                info!("Patched intent: {}. error was: {}", intent_id, err);
                err.to_string()
            })
            .collect();

        let callbacks_report = result
            .callbacks_report
            .iter()
            .map(|r| match r {
                Ok(sig) => {
                    format!("OK: {sig}")
                }
                Err(err) => {
                    error!(
                        "Callback failed to schedule: {}. error: {}",
                        intent_id, err
                    );
                    format!("ERR: {err}")
                }
            })
            .collect();

        SentCommit {
            message_id: intent_id,
            slot: intent_meta.slot,
            blockhash: intent_meta.blockhash,
            payer: intent_meta.payer,
            chain_signatures,
            included_pubkeys: intent_meta.included_pubkeys,
            excluded_pubkeys: vec![],
            requested_undelegation: intent_meta.requested_undelegation,
            error_message,
            patched_errors,
            callbacks_scheduling_results: callbacks_report,
        }
    }

    /// Retains bundles whose accounts are still delegated
    async fn retain_recoverable_intent_bundles(
        &self,
        bundles: &mut Vec<ScheduledIntentBundle>,
    ) {
        let results = join_all(
            bundles.iter().map(|b| b.get_all_committed_pubkeys()).map(
                |pubkeys| async move {
                    self.chainlink
                        .accounts_delegated_on_base_and_er(
                            &pubkeys,
                            AccountFetchOrigin::GetAccount,
                        )
                        .await
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

struct ScheduledBaseIntentMeta {
    slot: u64,
    blockhash: Hash,
    payer: Pubkey,
    included_pubkeys: Vec<Pubkey>,
    intent_sent_transaction: Transaction,
    requested_undelegation: bool,
}

impl ScheduledBaseIntentMeta {
    fn new(intent: &ScheduledIntentBundle) -> Self {
        Self {
            slot: intent.slot,
            blockhash: intent.blockhash,
            payer: intent.payer,
            included_pubkeys: intent.get_all_committed_pubkeys(),
            intent_sent_transaction: intent.sent_transaction.clone(),
            requested_undelegation: intent.has_undelegate_intent(),
        }
    }
}

#[derive(thiserror::Error, Debug)]
pub enum IntentExecutionServiceError {
    #[error("Invalid state: {0}")]
    InvalidState(String),
    #[error("JoinError: {0}")]
    JoinError(#[from] JoinError),
    #[error("IntentRpcClientError: {0}")]
    IntentRpcClientError(#[from] InternalOutboxClientError),
    #[error("OutboxReaderError")]
    OutboxReaderError(#[from] InternalOutboxIntentBundlesReaderError),
}
