use std::{
    collections::HashSet, future::Future, num::NonZeroUsize, sync::Arc,
    time::Duration,
};

use magicblock_chainlink::ProdChainlink;
use magicblock_core::intent::outbox::outbox_intent_pda_with_bump;
use magicblock_metrics::metrics::{self};
use magicblock_program::{Pubkey, outbox_intent_bundles::OutboxIntentBundle};
use nucleus::shutdown::{ShutdownHandle, ShutdownReason};
use solana_transaction::Transaction;
use tokio::task;
use tokio_util::sync::CancellationToken;
use tracing::error;

use crate::{
    committor_processor::CommittorProcessor,
    error::CommittorServiceResult,
    intent_engine::db::BacklogDB,
    intent_executor::error::IntentExecutorError,
    outbox::{
        OutboxClient,
        outbox_client::InternalOutboxClientError,
        outbox_intent_bundles_reader::{
            OutboxIntentBundlesReader, OutboxIntentBundlesReaderError,
        },
    },
};

pub type ChainlinkImpl = ProdChainlink;

pub struct IntentExecutionService<O, D> {
    /// Chainlink for notifying of undelegations
    chainlink: Arc<ChainlinkImpl>,
    /// ER client specific for Intent needs. Could be switched to RpcClient
    outbox_client: Arc<O>,
    /// Processor of accepted intents
    processor: Arc<CommittorProcessor<D>>,
    /// Time interval to scrape MagicContext(ER slot interval)
    // TODO(edwin): can be removed if LatestBlocK moved into magicblock-core
    slot_interval: Duration,
}

impl<O, D: BacklogDB> IntentExecutionService<O, D>
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
        processor: Arc<CommittorProcessor<D>>,
        slot_interval: Duration,
    ) -> Self {
        Self {
            chainlink,
            outbox_client,
            processor,
            slot_interval,
        }
    }

    pub async fn run(self, mut shutdown: ShutdownHandle) {
        let result = self.accept_worker(shutdown.child()).await;
        let reason = match result {
            Err(error) => ShutdownReason::Error(Box::new(error)),
            Ok(()) if shutdown.requested() => ShutdownReason::Signalled,
            Ok(()) => ShutdownReason::Unexpected,
        };
        shutdown.terminate(reason);
    }

    async fn accept_worker(
        self,
        cancellation_token: CancellationToken,
    ) -> Result<(), IntentExecutionServiceError> {
        // Reschedule existing outbox intents first
        // We need to ensure that accounts in outbox a scheduled before
        // we accept new incoming Intents
        if let Err(err) = self.reschedule_intents().await {
            // TODO(edwin): alerts
            error!(error = ?err, "Failed to reschedule pending bundles")
        }

        let mut interval = tokio::time::interval(self.slot_interval);
        loop {
            tokio::select! {
                biased;
                _ = cancellation_token.cancelled() => {
                    // Durable execution of intents makes its safe to exit right away
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

                    let intent_bundles = intent_bundles.into_iter().map(|bundle| {
                        let bump = outbox_intent_pda_with_bump(bundle.id).1;
                        OutboxIntentBundle::accepted(bundle, bump)
                    }).collect();
                    if let Err(err) = self.schedule_intent_execution(intent_bundles).await {
                        error!("Failed to schedule intent execution: {}", err);
                    }
                }
            }
        }
        Ok(())
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
            let mut intent_bundles_chunk = outbox_bundles_reader
                .read(RESCHEDULE_CHUNK_SIZE.get())
                .await
                .map_err(Into::into)?;
            if intent_bundles_chunk.is_empty() {
                return Ok(());
            }

            // Original blockhash is stale after restart; signal recovery so
            // notify_commit_sent rebuilds the tx with a fresh ER blockhash.
            intent_bundles_chunk.iter_mut().for_each(|b| {
                b.inner.sent_transaction = Transaction::default()
            });

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
    #[error("Intent execution worker '{0}' stopped unexpectedly")]
    WorkerStopped(&'static str),
    #[error("IntentRpcClientError: {0}")]
    IntentRpcClientError(#[from] InternalOutboxClientError),
    #[error("OutboxReaderError")]
    OutboxReaderError(#[from] OutboxIntentBundlesReaderError),
    #[error("IntentExecutorError: {0}")]
    IntentExecutorError(#[from] Box<IntentExecutorError>),
}
