use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use magicblock_account_cloner::ChainlinkCloner;
use magicblock_accounts_db::AccountsDb;
use magicblock_chainlink::{
    remote_account_provider::{
        chain_rpc_client::ChainRpcClientImpl,
        chain_updates_client::ChainUpdatesClient,
    },
    submux::SubMuxClient,
    Chainlink,
};
use magicblock_committor_service::{
    intent_execution_manager::BroadcastedIntentExecutionResult,
    intent_executor::ExecutionOutput, BaseIntentCommittor, CommittorService,
};
use magicblock_core::link::transactions::TransactionSchedulerHandle;
use magicblock_program::{
    magic_scheduled_base_intent::ScheduledIntentBundle,
    register_scheduled_commit_sent, SentCommit, TransactionScheduler,
};
use solana_hash::Hash;
use solana_pubkey::Pubkey;
use solana_transaction::Transaction;
use tokio::{
    sync::{broadcast, oneshot},
    task,
};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info};

use crate::{
    errors::ScheduledCommitsProcessorResult, ScheduledCommitsProcessor,
};

const POISONED_MUTEX_MSG: &str =
    "Mutex of RemoteScheduledCommitsProcessor.intents_meta_map is poisoned";

pub type ChainlinkImpl = Chainlink<
    ChainRpcClientImpl,
    SubMuxClient<ChainUpdatesClient>,
    AccountsDb,
    ChainlinkCloner,
>;

pub struct ScheduledCommitsProcessorImpl {
    committor: Arc<CommittorService>,
    chainlink: Arc<ChainlinkImpl>,
    cancellation_token: CancellationToken,
    intents_meta_map: Arc<Mutex<HashMap<u64, ScheduledBaseIntentMeta>>>,
    transaction_scheduler: TransactionScheduler,
}

impl ScheduledCommitsProcessorImpl {
    pub fn new(
        committor: Arc<CommittorService>,
        chainlink: Arc<ChainlinkImpl>,
        internal_transaction_scheduler: TransactionSchedulerHandle,
    ) -> Self {
        let result_subscriber = committor.subscribe_for_results();
        let intents_meta_map = Arc::new(Mutex::default());
        let cancellation_token = CancellationToken::new();
        tokio::spawn(Self::result_processor(
            result_subscriber,
            cancellation_token.clone(),
            intents_meta_map.clone(),
            internal_transaction_scheduler.clone(),
        ));

        Self {
            committor,
            chainlink,
            cancellation_token,
            intents_meta_map,
            transaction_scheduler: TransactionScheduler::default(),
        }
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
                "Failed to subscribe to accounts being undelegated: {:?}",
                sub_errors
            );
        }
    }

    async fn result_processor(
        result_subscriber: oneshot::Receiver<
            broadcast::Receiver<BroadcastedIntentExecutionResult>,
        >,
        cancellation_token: CancellationToken,
        intents_meta_map: Arc<Mutex<HashMap<u64, ScheduledBaseIntentMeta>>>,
        internal_transaction_scheduler: TransactionSchedulerHandle,
    ) {
        const SUBSCRIPTION_ERR_MSG: &str =
            "Failed to get subscription of results of BaseIntents execution";

        let mut result_receiver =
            result_subscriber.await.expect(SUBSCRIPTION_ERR_MSG);
        loop {
            let execution_result = tokio::select! {
                biased;
                _ = cancellation_token.cancelled() => {
                    info!("ScheduledCommitsProcessorImpl stopped.");
                    return;
                }
                execution_result = result_receiver.recv() => {
                    match execution_result {
                        Ok(result) => result,
                        Err(broadcast::error::RecvError::Closed) => {
                            info!("Intent execution got shutdown, shutting down result processor!");
                            break;
                        }
                        Err(broadcast::error::RecvError::Lagged(skipped)) => {
                            // SAFETY: This shouldn't happen as our tx execution is faster than Intent execution on Base layer
                            // If this ever happens it requires investigation
                            error!("ScheduledCommitsProcessorImpl lags behind Intent execution! skipped: {}", skipped);
                            continue;
                        }
                    }
                }
            };

            let intent_id = execution_result.id;
            // Remove intent from metas
            let intent_meta = if let Some(intent_meta) = intents_meta_map
                .lock()
                .expect(POISONED_MUTEX_MSG)
                .remove(&intent_id)
            {
                intent_meta
            } else {
                // Possible if we have duplicate Intents
                // First one will remove id from map and second could fail.
                // This should not happen and needs investigation!
                error!(
                    "CRITICAL! Failed to find IntentMeta for id: {}!",
                    intent_id
                );
                continue;
            };

            Self::process_intent_result(
                intent_id,
                &internal_transaction_scheduler,
                execution_result,
                intent_meta,
            )
            .await;
        }
    }

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
        match internal_transaction_scheduler
            .execute(intent_sent_transaction)
            .await
        {
            Ok(signature) => debug!(
                "Signaled sent commit with internal signature: {:?}",
                signature
            ),
            Err(err) => {
                error!("Failed to signal sent commit via transaction: {}", err);
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
                    .unwrap_or(vec![])
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
        }
    }
}

#[async_trait]
impl ScheduledCommitsProcessor for ScheduledCommitsProcessorImpl {
    async fn process(&self) -> ScheduledCommitsProcessorResult<()> {
        let intent_bundles =
            self.transaction_scheduler.take_scheduled_intent_bundles();

        if intent_bundles.is_empty() {
            return Ok(());
        }

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
        self.committor
            .schedule_intent_bundles(intent_bundles)
            .await??;
        Ok(())
    }

    fn scheduled_commits_len(&self) -> usize {
        self.transaction_scheduler.scheduled_actions_len()
    }

    fn clear_scheduled_commits(&self) {
        self.transaction_scheduler.clear_scheduled_actions();
    }

    fn stop(&self) {
        self.cancellation_token.cancel();
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
            intent_sent_transaction: intent
                .intent_bundle_sent_transaction
                .clone(),
            requested_undelegation: intent.has_undelegate_intent(),
        }
    }
}
