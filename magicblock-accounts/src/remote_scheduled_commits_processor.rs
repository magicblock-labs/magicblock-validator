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
    types::{ScheduledBaseIntentWrapper, TriggerType},
    BaseIntentCommittor,
};
use magicblock_processor::execute_transaction::execute_legacy_transaction;
use magicblock_program::{
    magic_scheduled_base_intent::{CommittedAccountV2, ScheduledBaseIntent},
    register_scheduled_commit_sent, FeePayerAccount, SentCommit,
    TransactionScheduler,
};
use magicblock_transaction_status::TransactionStatusSender;
use solana_sdk::{
    account::{Account, ReadableAccount},
    hash::Hash,
    pubkey::Pubkey,
    signature::Signature,
    system_program,
    transaction::Transaction,
};
use tokio::sync::{broadcast, oneshot};

use crate::{errors::AccountsResult, ScheduledCommitsProcessor};

const POISONED_RWLOCK_MSG: &str =
    "RwLock of RemoteAccountClonerWorker.last_clone_output is poisoned";
const POISONED_MUTEX_MSG: &str =
    "Mutex of RemoteScheduledCommitsProcessor.intents_meta_map is poisoned";

pub struct RemoteScheduledCommitsProcessor<C: BaseIntentCommittor> {
    bank: Arc<Bank>,
    committor: Arc<C>,
    intents_meta_map: Arc<Mutex<HashMap<u64, ScheduledBaseIntentMeta>>>,
    cloned_accounts: CloneOutputMap,
    transaction_scheduler: TransactionScheduler,
}

impl<C: BaseIntentCommittor> RemoteScheduledCommitsProcessor<C> {
    pub fn new(
        bank: Arc<Bank>,
        cloned_accounts: CloneOutputMap,
        committor: Arc<C>,
        transaction_status_sender: TransactionStatusSender,
    ) -> Self {
        let result_subscriber = committor.subscribe_for_results();
        let intents_meta_map = Arc::new(Mutex::default());
        tokio::spawn(Self::result_processor(
            bank.clone(),
            result_subscriber,
            intents_meta_map.clone(),
            transaction_status_sender,
        ));

        Self {
            bank,
            committor,
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
                account: &mut CommittedAccountV2,
            ) -> bool {
                let pubkey = account.pubkey;
                let ephemeral_pubkey =
                    AccountChainSnapshot::ephemeral_balance_pda(&pubkey);
                self.feepayers.insert(FeePayerAccount {
                    pubkey,
                    delegated_pda: ephemeral_pubkey,
                });

                // We commit escrow, its data kept under FeePayer's address
                match self.bank.get_account(&pubkey) {
                    Some(account_data) => {
                        account.pubkey = ephemeral_pubkey;
                        account.account = Account {
                            lamports: account_data.lamports(),
                            data: account_data.data().to_vec(),
                            owner: system_program::id(),
                            executable: account_data.executable(),
                            rent_epoch: account_data.rent_epoch(),
                        };
                        true
                    }
                    None => {
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
        intents_meta_map: Arc<Mutex<HashMap<u64, ScheduledBaseIntentMeta>>>,
        transaction_status_sender: TransactionStatusSender,
    ) {
        const SUBSCRIPTION_ERR_MSG: &str =
            "Failed to get subscription of results of BaseIntents execution";
        const META_ABSENT_ERR_MSG: &str =
            "Absent meta for executed intent should not be possible!";

        let mut result_receiver =
            result_subscriber.await.expect(SUBSCRIPTION_ERR_MSG);
        while let Ok(execution_result) = result_receiver.recv().await {
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
            let intent_meta = intents_meta_map
                .lock()
                .expect(POISONED_MUTEX_MSG)
                .remove(&intent_id)
                .expect(META_ABSENT_ERR_MSG);
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
                        &magicblock_committor_service::intent_executor::error::Error::EmptyIntentError => {
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
                            todo!()
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
        intent_meta: ScheduledBaseIntentMeta,
    ) {
        let chain_signatures = vec![
            execution_outcome.output.commit_signature,
            execution_outcome.output.finalize_signature,
        ];
        let sent_commit =
            Self::build_sent_commit(intent_id, chain_signatures, &intent_meta);
        register_scheduled_commit_sent(sent_commit);
        match execute_legacy_transaction(
            intent_meta.intent_sent_transaction,
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
        intent_meta: ScheduledBaseIntentMeta,
    ) {
        let sent_commit =
            Self::build_sent_commit(intent_id, vec![], &intent_meta);
        register_scheduled_commit_sent(sent_commit);
        match execute_legacy_transaction(
            intent_meta.intent_sent_transaction,
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
        intent_meta: &ScheduledBaseIntentMeta,
    ) -> SentCommit {
        SentCommit {
            message_id: intent_id,
            slot: intent_meta.slot,
            blockhash: intent_meta.blockhash,
            payer: intent_meta.payer,
            chain_signatures,
            included_pubkeys: intent_meta.included_pubkeys.clone(),
            excluded_pubkeys: intent_meta.excluded_pubkeys.clone(),
            feepayers: intent_meta.feepayers.clone(),
            requested_undelegation: intent_meta.requested_undelegation,
        }
    }
}

#[async_trait]
impl<C: BaseIntentCommittor> ScheduledCommitsProcessor
    for RemoteScheduledCommitsProcessor<C>
{
    async fn process(&self) -> AccountsResult<()> {
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

        self.committor.commit_base_intent(intents);
        Ok(())
    }

    fn scheduled_commits_len(&self) -> usize {
        self.transaction_scheduler.scheduled_actions_len()
    }

    fn clear_scheduled_commits(&self) {
        self.transaction_scheduler.clear_scheduled_actions();
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
