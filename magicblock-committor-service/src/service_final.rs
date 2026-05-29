use std::{
    collections::{HashMap, HashSet},
    mem,
    sync::{Arc, Mutex},
    time::Duration,
};

use async_trait::async_trait;
use magicblock_account_cloner::ChainlinkCloner;
use magicblock_accounts_db::{traits::AccountsBank, AccountsDb};
use magicblock_chainlink::{
    remote_account_provider::{
        chain_rpc_client::ChainRpcClientImpl,
        chain_updates_client::ChainUpdatesClient,
    },
    submux::SubMuxClient,
    Chainlink,
};
use magicblock_core::{
    link::transactions::{with_encoded, TransactionSchedulerHandle},
    traits::LatestBlockProvider,
};
use magicblock_metrics::metrics;
use magicblock_program::{
    instruction_utils::InstructionUtils,
    magic_scheduled_base_intent::ScheduledIntentBundle,
    register_scheduled_commit_sent, MagicContext, Pubkey, SentCommit,
    TransactionScheduler, MAGIC_CONTEXT_PUBKEY,
};
use solana_account::ReadableAccount;
use solana_hash::Hash;
use solana_transaction::Transaction;
use solana_transaction_error::TransactionError;
use tokio::{
    sync::broadcast,
    task,
    task::{JoinError, JoinHandle},
};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, instrument};

use crate::{
    committor_processor::CommittorProcessor, error::CommittorServiceError,
    intent_execution_manager::BroadcastedIntentExecutionResult,
    intent_executor::ExecutionOutput,
};

// TODO(edwin): adapt
const POISONED_MUTEX_MSG: &str =
    "Mutex of RemoteScheduledCommitsProcessor.intents_meta_map is poisoned";

pub type ChainlinkImpl = Chainlink<
    ChainRpcClientImpl,
    SubMuxClient<ChainUpdatesClient>,
    AccountsDb,
    ChainlinkCloner,
>;

#[async_trait]
pub trait IntentRpcClient: Send + Sync + 'static {
    type Error: std::error::Error + Send;

    /// Executes `Accept` tx and returns accepted intents
    async fn accept_scheduled_intents(
        &self,
    ) -> Result<Vec<ScheduledIntentBundle>, Self::Error>;

    /// Processes intent results, submitting them on chain(ER)
    async fn finalize_intent(
        &self,
        sent_tx: Transaction,
        sent_commit: SentCommit,
    ) -> Result<(), Self::Error>;
}

pub struct InternalIntentRpcClient<L: LatestBlockProvider> {
    /// Provides access to MagicContext
    accounts_db: Arc<AccountsDb>,
    /// Internal endpoint for scheduling ER TXs
    transaction_scheduler: TransactionSchedulerHandle,
    /// Provides access to ER latest block for TX creation
    latest_block_provider: L,
}

impl<L: LatestBlockProvider> InternalIntentRpcClient<L> {
    pub fn new(
        accounts_db: Arc<AccountsDb>,
        transaction_scheduler: TransactionSchedulerHandle,
        latest_block_provider: L,
    ) -> Self {
        Self {
            accounts_db,
            transaction_scheduler,
            latest_block_provider,
        }
    }

    /// Sends transaction to move the scheduled commits from the `MagicContext`
    /// to the global ScheduledCommit store
    async fn execute_accept_tx(&self) -> Result<(), InternalRpcClientError> {
        let tx = InstructionUtils::accept_scheduled_commits(
            self.latest_block_provider.blockhash(),
        );
        let encoded_tx = with_encoded(tx).inspect_err(|err| {
            error!(error = ?err, "Failed to bincode intent transaction");
        })?;
        self.transaction_scheduler
            .execute(encoded_tx)
            .await
            .inspect_err(|err| {
                error!(error = ?err, "Failed to accept scheduled commits");
            })?;

        Ok(())
    }
}

#[async_trait]
impl<L: LatestBlockProvider> IntentRpcClient for InternalIntentRpcClient<L> {
    type Error = InternalRpcClientError;

    async fn accept_scheduled_intents(
        &self,
    ) -> Result<Vec<ScheduledIntentBundle>, Self::Error> {
        // If accounts were scheduled to be committed, we accept them here
        // and processs the commits
        let magic_context_acc =
            self.accounts_db.get_account(&MAGIC_CONTEXT_PUBKEY).expect(
                "Validator found to be running without MagicContext account!",
            );
        if !MagicContext::has_scheduled_commits(magic_context_acc.data()) {
            return Ok(vec![]);
        }
        self.execute_accept_tx().await?;

        // Return intents from global store
        Ok(TransactionScheduler::default().take_scheduled_intent_bundles())
    }

    async fn finalize_intent(
        &self,
        sent_tx: Transaction,
        sent_commit: SentCommit,
    ) -> Result<(), Self::Error> {
        register_scheduled_commit_sent(sent_commit);
        let txn = with_encoded(sent_tx).inspect_err(|err| {
            // Unreachable case, all intent transactions are smaller than 64KB by construction
            error!(error = ?err, "Failed to bincode intent transaction");
        })?;
        self.transaction_scheduler
            .execute(txn)
            .await
            .inspect(|_| debug!("Sent commit signaled"))
            .inspect_err(
                |err| error!(error = ?err, "Failed to signal sent commit"),
            )?;

        Ok(())
    }
}

#[derive(thiserror::Error, Debug)]
pub enum InternalRpcClientError {
    // TODO(edwin)
    #[error("asd")]
    TransactionError(#[from] TransactionError),
}

pub enum IntentExecutionService<R> {
    Created(ServiceInner<R>),
    Started(JoinHandle<()>),
    Stopped,
    Error,
}

impl<R: IntentRpcClient> IntentExecutionService<R> {
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

    pub fn start(&mut self) {
        let Self::Created(service) = self.take() else {
            // TODO(edwin): err InvalidState(String)
            return;
        };

        let handle = service.start();
        *self = Self::Started(handle);
    }

    pub async fn stop(&mut self) -> Result<(), JoinError> {
        let Self::Started(handle) = self.take() else {
            // TODO(edwin): err InvalidState(String)
            return Ok(());
        };

        handle.await?;
        *self = Self::Stopped;
        Ok(())
    }
}

pub struct ServiceInner<R> {
    /// Chainlink for notifying of undelegations
    chainlink: Arc<ChainlinkImpl>,
    /// ER client specific for Intent needs
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

impl<R: IntentRpcClient> ServiceInner<R> {
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

    pub fn start(self) -> JoinHandle<()> {
        tokio::task::spawn(self.run())
    }

    async fn run(self) {
        // TODO(edwin): move to start and make ExecutionHandle{main, result_worker}
        let result_subscriber = self.processor.subscribe_for_results();
        let cancellation_token = self.cancellation_token.clone();
        let handle = tokio::spawn(Self::result_processor(
            result_subscriber,
            cancellation_token,
            self.intents_meta_map.clone(),
            self.intent_rpc_client.clone(),
        ));

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

        // TODO(edwin): handle cancellation
        todo!()
    }

    async fn schedule_intent_execution(
        &self,
        intent_bundles: Vec<ScheduledIntentBundle>,
    ) -> Result<(), CommittorServiceError> {
        if intent_bundles.is_empty() {
            return Ok(());
        }

        metrics::inc_committor_intents_count_by(intent_bundles.len() as u64);

        // Add metas for intent we schedule
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

            pubkeys_being_undelegated.into_iter().collect::<Vec<_>>()
        };

        self.process_undelegation_requests(pubkeys_being_undelegated)
            .await;
        self.processor
            .schedule_intent_bundles(intent_bundles)
            .await?;
        Ok(())
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
        const SUBSCRIPTION_ERR_MSG: &str =
            "Failed to get subscription of results of BaseIntents execution";

        loop {
            let execution_result = tokio::select! {
                biased;
                _ = cancellation_token.cancelled() => {
                    // TODO(edwin): validate shutdown correctness
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

            ServiceInner::<R>::process_execution_result(
                &intent_client,
                execution_result,
                &intents_meta_map,
            )
            .await;
        }
    }

    async fn process_execution_result(
        intent_client: &Arc<R>,
        execution_result: BroadcastedIntentExecutionResult,
        intents_meta_map: &Arc<Mutex<HashMap<u64, ScheduledBaseIntentMeta>>>,
    ) -> Result<(), IntentExecutionServiceError> {
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
        let sent_commit = ServiceInner::<R>::build_sent_commit(
            intent_id,
            intent_meta,
            execution_result,
        );
        // TODO(edwin): convert
        intent_client
            .finalize_intent(sent_transaction, sent_commit)
            .await;

        Ok(())
    }

    #[instrument(
        skip(internal_transaction_scheduler, result, intent_meta),
        fields(intent_id)
    )]
    async fn process_intent_result(
        intent_id: u64,
        internal_transaction_scheduler: &TransactionSchedulerHandle,
        result: BroadcastedIntentExecutionResult,
        mut intent_meta: ScheduledBaseIntentMeta,
    ) {
        let intent_sent_transaction =
            std::mem::take(&mut intent_meta.intent_sent_transaction);
        let sent_commit =
            Self::build_sent_commit(intent_id, intent_meta, result);
        register_scheduled_commit_sent(sent_commit);
        let Ok(txn) = with_encoded(intent_sent_transaction) else {
            // Unreachable case, all intent transactions are smaller than 64KB by construction
            error!("Failed to bincode intent transaction");
            return;
        };
        match internal_transaction_scheduler.execute(txn).await {
            Ok(()) => {
                debug!("Sent commit signaled")
            }
            Err(err) => {
                error!(error = ?err, "Failed to signal sent commit");
            }
        }
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
pub enum IntentExecutionServiceError {}
