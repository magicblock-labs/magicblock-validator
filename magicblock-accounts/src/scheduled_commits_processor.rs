use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use conjunto_transwise::AccountChainSnapshot;
use log::{debug, error, info, warn};
use magicblock_account_cloner::{AccountClonerOutput, CloneOutputMap};
use magicblock_bank::bank::Bank;
use magicblock_committor_service::{
    intent_execution_manager::{
        BroadcastedIntentExecutionResult, ExecutionOutputWrapper,
    },
    intent_executor::ExecutionOutput,
    types::{ScheduledBaseIntentWrapper, TriggerType},
    BaseIntentCommittor,
};
use magicblock_processor::execute_transaction::execute_legacy_transaction;
use magicblock_program::{
    magic_scheduled_base_intent::{CommittedAccount, ScheduledBaseIntent},
    register_scheduled_commit_sent, FeePayerAccount, SentCommit,
    TransactionScheduler,
};
use magicblock_transaction_status::TransactionStatusSender;
use solana_sdk::{
    hash::Hash, pubkey::Pubkey, signature::Signature, transaction::Transaction,
};
use tokio::sync::{broadcast, oneshot};
use tokio_util::sync::CancellationToken;

use crate::{
    errors::ScheduledCommitsProcessorResult, ScheduledCommitsProcessor,
};

const POISONED_RWLOCK_MSG: &str =
    "RwLock of RemoteAccountClonerWorker.last_clone_output is poisoned";
const POISONED_MUTEX_MSG: &str =
    "Mutex of RemoteScheduledCommitsProcessor.intents_meta_map is poisoned";

pub struct ScheduledCommitsProcessorImpl<C: BaseIntentCommittor> {
    bank: Arc<Bank>,
    committor: Arc<C>,
    cancellation_token: CancellationToken,
    intents_meta_map: Arc<Mutex<HashMap<u64, ScheduledBaseIntentMeta>>>,
    cloned_accounts: CloneOutputMap,
    transaction_scheduler: TransactionScheduler,
}

impl<C: BaseIntentCommittor> ScheduledCommitsProcessorImpl<C> {
    pub fn new(
        bank: Arc<Bank>,
        cloned_accounts: CloneOutputMap,
        committor: Arc<C>,
        transaction_status_sender: TransactionStatusSender,
    ) -> Self {
        let result_subscriber = committor.subscribe_for_results();
        let intents_meta_map = Arc::new(Mutex::default());
        let cancellation_token = CancellationToken::new();
        tokio::spawn(Self::result_processor(
            bank.clone(),
            result_subscriber,
            cancellation_token.clone(),
            intents_meta_map.clone(),
            transaction_status_sender,
        ));

        Self {
            bank,
            committor,
            cancellation_token,
            intents_meta_map,
            cloned_accounts,
            transaction_scheduler: TransactionScheduler::default(),
        }
    }

    fn preprocess_intent(
        &self,
        mut base_intent: ScheduledBaseIntent,
    ) -> (
        ScheduledBaseIntentWrapper,
        Vec<Pubkey>,
        HashSet<FeePayerAccount>,
    ) {
        let Some(committed_accounts) = base_intent.get_committed_accounts_mut()
        else {
            let intent = ScheduledBaseIntentWrapper {
                inner: base_intent,
                trigger_type: TriggerType::OnChain,
            };
            return (intent, vec![], HashSet::new());
        };

        struct Processor<'a> {
            excluded_pubkeys: HashSet<Pubkey>,
            feepayers: HashSet<FeePayerAccount>,
            bank: &'a Bank,
        }

        impl Processor<'_> {
            /// Handles case when committed account is feepayer
            /// Returns `true` if account should be retained, `false` otherwise
            fn process_feepayer(
                &mut self,
                account: &mut CommittedAccount,
            ) -> bool {
                let pubkey = account.pubkey;
                let ephemeral_pubkey =
                    AccountChainSnapshot::ephemeral_balance_pda(&pubkey);
                self.feepayers.insert(FeePayerAccount {
                    pubkey,
                    delegated_pda: ephemeral_pubkey,
                });

                // We commit escrow, its data kept under FeePayer's address
                if let Some(account_data) = self.bank.get_account(&pubkey) {
                    account.pubkey = ephemeral_pubkey;
                    account.account = account_data.into();
                    true
                } else {
                    // TODO(edwin): shouldn't be possible.. Should be a panic
                    error!(
                        "Scheduled commit account '{}' not found. It must have gotten undelegated and removed since it was scheduled.",
                        pubkey
                    );
                    self.excluded_pubkeys.insert(pubkey);
                    false
                }
            }
        }

        let mut processor = Processor {
            excluded_pubkeys: HashSet::new(),
            feepayers: HashSet::new(),
            bank: &self.bank,
        };

        // Retains onlu account that are valid to be commited
        committed_accounts.retain_mut(|account| {
            let pubkey = account.pubkey;
            let cloned_accounts =
                self.cloned_accounts.read().expect(POISONED_RWLOCK_MSG);
            let account_chain_snapshot = match cloned_accounts.get(&pubkey) {
                Some(AccountClonerOutput::Cloned {
                    account_chain_snapshot,
                    ..
                }) => account_chain_snapshot,
                Some(AccountClonerOutput::Unclonable { .. }) => {
                    error!("Unclonable account as part of commit");
                    return false;
                }
                None => {
                    error!("Account snapshot is absent during commit!");
                    return false;
                }
            };

            if account_chain_snapshot.chain_state.is_feepayer() {
                // Feepayer case, should actually always return true
                processor.process_feepayer(account)
            } else if account_chain_snapshot.chain_state.is_undelegated() {
                // Can be safely excluded
                processor.excluded_pubkeys.insert(account.pubkey);
                false
            } else {
                // Means delegated so we keep it
                true
            }
        });

        let feepayers = processor.feepayers;
        let excluded_pubkeys = processor.excluded_pubkeys.into_iter().collect();
        let intent = ScheduledBaseIntentWrapper {
            inner: base_intent,
            trigger_type: TriggerType::OnChain,
        };

        (intent, excluded_pubkeys, feepayers)
    }

    async fn result_processor(
        bank: Arc<Bank>,
        result_subscriber: oneshot::Receiver<
            broadcast::Receiver<BroadcastedIntentExecutionResult>,
        >,
        cancellation_token: CancellationToken,
        intents_meta_map: Arc<Mutex<HashMap<u64, ScheduledBaseIntentMeta>>>,
        transaction_status_sender: TransactionStatusSender,
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
                        &bank,
                        &transaction_status_sender,
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
                                &bank,
                                &transaction_status_sender,
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
        bank: &Arc<Bank>,
        transaction_status_sender: &TransactionStatusSender,
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
        match execute_legacy_transaction(
            intent_sent_transaction,
            bank,
            Some(transaction_status_sender),
        ) {
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
        bank: &Arc<Bank>,
        transaction_status_sender: &TransactionStatusSender,
        mut intent_meta: ScheduledBaseIntentMeta,
    ) {
        let intent_sent_transaction =
            std::mem::take(&mut intent_meta.intent_sent_transaction);
        let sent_commit =
            Self::build_sent_commit(intent_id, vec![], intent_meta);
        register_scheduled_commit_sent(sent_commit);
        match execute_legacy_transaction(
            intent_sent_transaction,
            bank,
            Some(transaction_status_sender),
        ) {
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
            feepayers: intent_meta.feepayers,
            requested_undelegation: intent_meta.requested_undelegation,
        }
    }
}

#[async_trait]
impl<C: BaseIntentCommittor> ScheduledCommitsProcessor
    for ScheduledCommitsProcessorImpl<C>
{
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
        let intents = {
            let mut intent_metas =
                self.intents_meta_map.lock().expect(POISONED_MUTEX_MSG);

            intents
                .map(|(intent, excluded_pubkeys, feepayers)| {
                    intent_metas.insert(
                        intent.id,
                        ScheduledBaseIntentMeta::new(
                            &intent,
                            excluded_pubkeys,
                            feepayers,
                        ),
                    );

                    intent
                })
                .collect()
        };

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
    feepayers: HashSet<FeePayerAccount>,
    intent_sent_transaction: Transaction,
    requested_undelegation: bool,
}

impl ScheduledBaseIntentMeta {
    fn new(
        intent: &ScheduledBaseIntent,
        excluded_pubkeys: Vec<Pubkey>,
        feepayers: HashSet<FeePayerAccount>,
    ) -> Self {
        Self {
            slot: intent.slot,
            blockhash: intent.blockhash,
            payer: intent.payer,
            included_pubkeys: intent
                .get_committed_pubkeys()
                .unwrap_or_default(),
            excluded_pubkeys,
            feepayers,
            intent_sent_transaction: intent.action_sent_transaction.clone(),
            requested_undelegation: intent.is_undelegate(),
        }
    }
}
