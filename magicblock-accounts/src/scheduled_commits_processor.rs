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
use magicblock_core::link::transactions::{
    with_encoded, TransactionSchedulerHandle,
};
use magicblock_metrics::metrics;
use magicblock_program::{
    instruction_utils::InstructionUtils,
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
use tracing::{debug, error, info, instrument};

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
        tokio::spawn(Self::recover_pending_intents(
            committor.clone(),
            chainlink.clone(),
            intents_meta_map.clone(),
        ));

        Self {
            committor,
            chainlink,
            cancellation_token,
            intents_meta_map,
            transaction_scheduler: TransactionScheduler::default(),
        }
    }

    async fn process_undelegation_requests(
        chainlink: Arc<ChainlinkImpl>,
        pubkeys: Vec<Pubkey>,
    ) {
        let mut join_set = task::JoinSet::new();
        for pubkey in pubkeys.into_iter() {
            let chainlink = chainlink.clone();
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

    #[instrument(skip(committor, chainlink, intents_meta_map))]
    async fn recover_pending_intents(
        committor: Arc<CommittorService>,
        chainlink: Arc<ChainlinkImpl>,
        intents_meta_map: Arc<Mutex<HashMap<u64, ScheduledBaseIntentMeta>>>,
    ) {
        let intent_bundles = match committor.get_pending_intent_bundles().await
        {
            Ok(Ok(intent_bundles)) => intent_bundles,
            Ok(Err(err)) => {
                error!(error = ?err, "Failed to load pending commit intents");
                return;
            }
            Err(err) => {
                error!(error = ?err, "Failed to receive pending commit intents");
                return;
            }
        };
        if intent_bundles.is_empty() {
            return;
        }

        let pubkeys_being_undelegated = {
            let mut intent_metas = match intents_meta_map.lock() {
                Ok(intent_metas) => intent_metas,
                Err(err) => {
                    error!(
                        error = %err,
                        "Failed to register recovered commit intent metadata"
                    );
                    return;
                }
            };
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

        Self::process_undelegation_requests(
            chainlink,
            pubkeys_being_undelegated,
        )
        .await;

        let total_count = intent_bundles.len();
        let mut scheduled_count = 0;
        for intent_bundle in intent_bundles {
            let intent_id = intent_bundle.id;
            match committor
                .schedule_recovered_intent_bundles(vec![intent_bundle])
                .await
            {
                Ok(Ok(())) => {
                    scheduled_count += 1;
                }
                Ok(Err(err)) => {
                    Self::remove_intent_metas(&intents_meta_map, &[intent_id]);
                    error!(intent_id, error = ?err, "Failed to schedule recovered pending commit intent");
                }
                Err(err) => {
                    Self::remove_intent_metas(&intents_meta_map, &[intent_id]);
                    error!(intent_id, error = ?err, "Failed to receive recovered pending commit intent schedule result");
                }
            }
        }

        if scheduled_count > 0 {
            info!(
                intent_count = scheduled_count,
                total_count, "Scheduled recovered pending commit intents"
            );
        }
    }

    fn remove_intent_metas(
        intents_meta_map: &Arc<Mutex<HashMap<u64, ScheduledBaseIntentMeta>>>,
        intent_ids: &[u64],
    ) {
        let mut intent_metas = match intents_meta_map.lock() {
            Ok(intent_metas) => intent_metas,
            Err(err) => {
                error!(
                    error = %err,
                    "Recovered commit intent metadata map was poisoned"
                );
                err.into_inner()
            }
        };
        for intent_id in intent_ids {
            intent_metas.remove(intent_id);
        }
    }

    #[instrument(skip(
        result_subscriber,
        cancellation_token,
        intents_meta_map,
        internal_transaction_scheduler
    ))]
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
                    info!("Shutting down");
                    return;
                }
                execution_result = result_receiver.recv() => {
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
                error!(intent_id, "Failed to find intent metadata");
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
        let intent_sent_transaction = intent_meta
            .intent_sent_transaction
            .take()
            .unwrap_or_else(|| {
                InstructionUtils::scheduled_commit_sent(
                    intent_id,
                    intent_meta.blockhash,
                )
            });
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

#[async_trait]
impl ScheduledCommitsProcessor for ScheduledCommitsProcessorImpl {
    #[instrument(skip(self))]
    async fn process(&self) -> ScheduledCommitsProcessorResult<()> {
        let intent_bundles =
            self.transaction_scheduler.take_scheduled_intent_bundles();

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

        Self::process_undelegation_requests(
            self.chainlink.clone(),
            pubkeys_being_undelegated,
        )
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
    intent_sent_transaction: Option<Transaction>,
    requested_undelegation: bool,
}

impl ScheduledBaseIntentMeta {
    fn new(intent: &ScheduledIntentBundle) -> Self {
        Self {
            slot: intent.slot,
            blockhash: intent.blockhash,
            payer: intent.payer,
            included_pubkeys: intent.get_all_committed_pubkeys(),
            intent_sent_transaction: if intent
                .sent_transaction
                .signatures
                .is_empty()
            {
                None
            } else {
                Some(intent.sent_transaction.clone())
            },
            requested_undelegation: intent.has_undelegate_intent(),
        }
    }
}
