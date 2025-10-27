use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use log::{debug, error, info, warn};
use magicblock_account_cloner::ChainlinkCloner;
use magicblock_accounts_db::AccountsDb;
use magicblock_chainlink::{
    remote_account_provider::{
        chain_pubsub_client::ChainPubsubClientImpl,
        chain_rpc_client::ChainRpcClientImpl,
    },
    submux::SubMuxClient,
    Chainlink,
};
use magicblock_committor_service::{
    intent_execution_manager::{
        BroadcastedIntentExecutionResult, ExecutionOutputWrapper,
    },
    intent_executor::ExecutionOutput,
    types::{ScheduledBaseIntentWrapper, TriggerType},
    BaseIntentCommittor, CommittorService,
};
use magicblock_core::{
    link::transactions::TransactionSchedulerHandle, traits::AccountsBank,
};
use magicblock_program::{
    magic_scheduled_base_intent::ScheduledBaseIntent,
    register_scheduled_commit_sent, SentCommit, TransactionScheduler,
};
use solana_sdk::{
    hash::Hash, pubkey::Pubkey, signature::Signature, transaction::Transaction,
};
use tokio::{
    sync::{broadcast, oneshot},
    task,
};
use tokio_util::sync::CancellationToken;

use crate::{
    errors::ScheduledCommitsProcessorResult, ScheduledCommitsProcessor,
};

const POISONED_MUTEX_MSG: &str =
    "Mutex of RemoteScheduledCommitsProcessor.intents_meta_map is poisoned";

pub type ChainlinkImpl = Chainlink<
    ChainRpcClientImpl,
    SubMuxClient<ChainPubsubClientImpl>,
    AccountsDb,
    ChainlinkCloner,
>;

pub struct ScheduledCommitsProcessorImpl {
    accounts_bank: Arc<AccountsDb>,
    committor: Arc<CommittorService>,
    chainlink: Arc<ChainlinkImpl>,
    cancellation_token: CancellationToken,
    intents_meta_map: Arc<Mutex<HashMap<u64, ScheduledBaseIntentMeta>>>,
    transaction_scheduler: TransactionScheduler,
}

impl ScheduledCommitsProcessorImpl {
    pub fn new(
        accounts_bank: Arc<AccountsDb>,
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
            accounts_bank,
            committor,
            chainlink,
            cancellation_token,
            intents_meta_map,
            transaction_scheduler: TransactionScheduler::default(),
        }
    }

    fn preprocess_intent(
        &self,
        mut base_intent: ScheduledBaseIntent,
    ) -> (ScheduledBaseIntentWrapper, Vec<Pubkey>, Vec<Pubkey>) {
        let is_undelegate = base_intent.is_undelegate();
        let Some(committed_accounts) = base_intent.get_committed_accounts_mut()
        else {
            let intent = ScheduledBaseIntentWrapper {
                inner: base_intent,
                trigger_type: TriggerType::OnChain,
            };
            return (intent, vec![], vec![]);
        };

        let mut excluded_pubkeys = vec![];
        let mut pubkeys_being_undelegated = vec![];
        // Retains only account that are valid to be committed (all delegated ones)
        committed_accounts.retain_mut(|account| {
            let pubkey = account.pubkey;
            let acc = self.accounts_bank.get_account(&pubkey);
            match acc {
                Some(acc) => {
                    if acc.delegated() {
                        if is_undelegate {
                            pubkeys_being_undelegated.push(pubkey);
                        }
                        true
                    } else {
                        excluded_pubkeys.push(pubkey);
                        false
                    }
                }
                None => {
                    warn!(
                        "Account {} not found in AccountsDb, skipping from commit",
                        pubkey
                    );
                    false
                }
            }
        });

        let intent = ScheduledBaseIntentWrapper {
            inner: base_intent,
            trigger_type: TriggerType::OnChain,
        };

        (intent, excluded_pubkeys, pubkeys_being_undelegated)
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

            let (intent_id, trigger_type) = execution_result
                .as_ref()
                .map(|output| (output.id, output.trigger_type))
                .unwrap_or_else(|(id, trigger_type, _)| (*id, *trigger_type));

            // Here we handle on OnChain triggered intent
            // TODO: should be removed once crank supported
            if matches!(trigger_type, TriggerType::OffChain) {
                info!("OffChain triggered BaseIntent executed: {}", intent_id);
                continue;
            }

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

            match execution_result {
                Ok(value) => {
                    Self::process_intent_result(
                        intent_id,
                        &internal_transaction_scheduler,
                        value,
                        intent_meta,
                    )
                    .await;
                }
                Err((_, _, err)) => {
                    match err.as_ref() {
                        &magicblock_committor_service::intent_executor::error::IntentExecutorError::EmptyIntentError => {
                            warn!("Empty intent was scheduled!");
                            Self::process_empty_intent(
                                intent_id,
                                &internal_transaction_scheduler,
                                intent_meta
                            ).await;
                        }
                        _ => {
                            error!("Failed to commit: {:?}", err);
                        }
                    }
                }
            }
        }
    }

    async fn process_intent_result(
        intent_id: u64,
        internal_transaction_scheduler: &TransactionSchedulerHandle,
        execution_outcome: ExecutionOutputWrapper,
        mut intent_meta: ScheduledBaseIntentMeta,
    ) {
        let chain_signatures = match execution_outcome.output {
            ExecutionOutput::SingleStage(signature) => vec![signature],
            ExecutionOutput::TwoStage {
                commit_signature,
                finalize_signature,
            } => vec![commit_signature, finalize_signature],
        };
        let intent_sent_transaction =
            std::mem::take(&mut intent_meta.intent_sent_transaction);
        let sent_commit =
            Self::build_sent_commit(intent_id, chain_signatures, intent_meta);
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

    async fn process_empty_intent(
        intent_id: u64,
        internal_transaction_scheduler: &TransactionSchedulerHandle,
        mut intent_meta: ScheduledBaseIntentMeta,
    ) {
        let intent_sent_transaction =
            std::mem::take(&mut intent_meta.intent_sent_transaction);
        let sent_commit =
            Self::build_sent_commit(intent_id, vec![], intent_meta);
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
        chain_signatures: Vec<Signature>,
        intent_meta: ScheduledBaseIntentMeta,
    ) -> SentCommit {
        SentCommit {
            message_id: intent_id,
            slot: intent_meta.slot,
            blockhash: intent_meta.blockhash,
            payer: intent_meta.payer,
            chain_signatures,
            included_pubkeys: intent_meta.included_pubkeys,
            excluded_pubkeys: intent_meta.excluded_pubkeys,
            requested_undelegation: intent_meta.requested_undelegation,
        }
    }
}

#[async_trait]
impl ScheduledCommitsProcessor for ScheduledCommitsProcessorImpl {
    async fn process(&self) -> ScheduledCommitsProcessorResult<()> {
        let scheduled_base_intent =
            self.transaction_scheduler.take_scheduled_actions();

        if scheduled_base_intent.is_empty() {
            return Ok(());
        }

        let intents = scheduled_base_intent
            .into_iter()
            .map(|intent| self.preprocess_intent(intent));

        // Add metas for intent we schedule
        let (intents, pubkeys_being_undelegated) = {
            let mut intent_metas =
                self.intents_meta_map.lock().expect(POISONED_MUTEX_MSG);
            let mut pubkeys_being_undelegated = HashSet::new();

            let intents = intents
                .map(|(intent, excluded_pubkeys, undelegated)| {
                    intent_metas.insert(
                        intent.id,
                        ScheduledBaseIntentMeta::new(&intent, excluded_pubkeys),
                    );
                    pubkeys_being_undelegated.extend(undelegated);

                    intent
                })
                .collect::<Vec<_>>();

            (
                intents,
                pubkeys_being_undelegated.into_iter().collect::<Vec<_>>(),
            )
        };

        self.process_undelegation_requests(pubkeys_being_undelegated)
            .await;
        self.committor.schedule_base_intent(intents).await??;
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
    excluded_pubkeys: Vec<Pubkey>,
    intent_sent_transaction: Transaction,
    requested_undelegation: bool,
}

impl ScheduledBaseIntentMeta {
    fn new(
        intent: &ScheduledBaseIntent,
        excluded_pubkeys: Vec<Pubkey>,
    ) -> Self {
        Self {
            slot: intent.slot,
            blockhash: intent.blockhash,
            payer: intent.payer,
            included_pubkeys: intent
                .get_committed_pubkeys()
                .unwrap_or_default(),
            excluded_pubkeys,
            intent_sent_transaction: intent.action_sent_transaction.clone(),
            requested_undelegation: intent.is_undelegate(),
        }
    }
}
