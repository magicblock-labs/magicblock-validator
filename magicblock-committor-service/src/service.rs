use std::{
    collections::HashSet, future::Future, mem, num::NonZeroUsize, sync::Arc,
    time::Duration,
};

use magicblock_account_cloner::ChainlinkCloner;
use magicblock_chainlink::{ProdChainlink, ProdInnerChainlink};
use magicblock_metrics::metrics::{self};
use magicblock_program::{outbox_intent_bundles::OutboxIntentBundle, Pubkey};
use tokio::{
    task,
    task::{JoinError, JoinHandle},
};
use tokio_util::sync::CancellationToken;
use tracing::error;

use crate::{
    committor_processor::CommittorProcessor,
    error::CommittorServiceResult,
    intent_executor::error::IntentExecutorError,
    outbox::{
        outbox_client::{InternalOutboxClientError, OutboxClient},
        outbox_intent_bundles_reader::{
            OutboxIntentBundlesReader, OutboxIntentBundlesReaderError,
        },
    },
};

pub type InnerChainlinkImpl = ProdInnerChainlink<ChainlinkCloner>;
pub type ChainlinkImpl = ProdChainlink<ChainlinkCloner>;

pub enum IntentExecutionService<O> {
    Created(ServiceInner<O>),
    Started(JoinHandle<()>),
    Stopped,
    Error,
}

impl<O> IntentExecutionService<O>
where
    O: OutboxClient,
    // OutboxClient errors should be convertible to Service errors
    O::Error: Into<IntentExecutionServiceError>,
    // OutboxClient errors should be convertible to IntentExecutor error
    O::Error: Into<IntentExecutorError>,
    // OutboxReader errors should be convertible to Service errors
    <O::OutboxReader as OutboxIntentBundlesReader>::Error:
        Into<IntentExecutionServiceError>,
{
    pub fn new(
        chainlink: Arc<ChainlinkImpl>,
        intent_client: Arc<O>,
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
}

impl<O> ServiceInner<O>
where
    O: OutboxClient,
    // OutboxClient errors should be convertible to Service errors
    O::Error: Into<IntentExecutionServiceError>,
    // OutboxClient errors should be convertible into IntentExecutor errors
    O::Error: Into<IntentExecutorError>,
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
        }
    }

    fn start(self) -> JoinHandle<()> {
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
                    let intent_bundles = accept_result.unwrap_or_else(|(accepted_intents, err)| {
                        error!("Failed to accept intents: {}", err);
                        accepted_intents
                    });

                    let intent_bundles= intent_bundles.into_iter().map(OutboxIntentBundle::accepted).collect();
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
                .read(RESCHEDULE_CHUNK_SIZE.get())
                .await
                .map_err(Into::into)?;
            if intent_bundles_chunk.is_empty() {
                return Ok(());
            }

            // TODO(edwin): use status
            let read_len = intent_bundles_chunk.len();
            // Schedule  without initial persistence as bundle already exists in db
            let result = self
                .process_intent_bundles(intent_bundles_chunk, |bundles| {
                    self.processor.schedule_intent_bundles(bundles)
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

    async fn schedule_intent_execution(
        &self,
        intent_bundles: Vec<OutboxIntentBundle>,
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
        intent_bundles: Vec<OutboxIntentBundle>,
        schedule: F,
    ) -> CommittorServiceResult<()>
    where
        F: FnOnce(Vec<OutboxIntentBundle>) -> Fut,
        Fut: Future<Output = CommittorServiceResult<()>>,
    {
        if intent_bundles.is_empty() {
            return Ok(());
        }

        let pubkeys_being_undelegated = {
            let mut pubkeys_being_undelegated = HashSet::<Pubkey>::new();
            intent_bundles.iter().for_each(|intent| {
                if let Some(undelegate) = intent.get_undelegate_intent_pubkeys()
                {
                    pubkeys_being_undelegated.extend(undelegate);
                }
            });
            pubkeys_being_undelegated.into_iter().collect::<Vec<_>>()
        };

        self.process_undelegation_requests(pubkeys_being_undelegated)
            .await;

        schedule(intent_bundles).await
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
    OutboxReaderError(#[from] OutboxIntentBundlesReaderError),
    #[error("asd")]
    ASdError(#[from] IntentExecutorError),
}
