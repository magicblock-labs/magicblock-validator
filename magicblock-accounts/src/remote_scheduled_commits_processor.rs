use std::{collections::HashSet, sync::Arc};

use async_trait::async_trait;
use conjunto_transwise::AccountChainSnapshot;
use log::{debug, error, info};
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
    register_scheduled_commit_sent, FeePayerAccount, TransactionScheduler,
};
use magicblock_transaction_status::TransactionStatusSender;
use solana_sdk::{
    account::{Account, ReadableAccount},
    pubkey::Pubkey,
    system_program,
};
use tokio::sync::{broadcast, oneshot};

use crate::{errors::AccountsResult, ScheduledCommitsProcessor};

const POISONED_RWLOCK_MSG: &str =
    "RwLock of RemoteAccountClonerWorker.last_clone_output is poisoned";

pub struct RemoteScheduledCommitsProcessor<C: BaseIntentCommittor> {
    transaction_scheduler: TransactionScheduler,
    cloned_accounts: CloneOutputMap,
    bank: Arc<Bank>,
    committor: Arc<C>,
}

impl<C: BaseIntentCommittor> RemoteScheduledCommitsProcessor<C> {
    pub fn new(
        bank: Arc<Bank>,
        cloned_accounts: CloneOutputMap,
        committor: Arc<C>,
        transaction_status_sender: TransactionStatusSender,
    ) -> Self {
        let result_subscriber = committor.subscribe_for_results();
        tokio::spawn(Self::result_processor(
            bank.clone(),
            result_subscriber,
            transaction_status_sender,
        ));

        Self {
            bank,
            cloned_accounts,
            committor,
            transaction_scheduler: TransactionScheduler::default(),
        }
    }

    fn preprocess_intent(
        &self,
        mut base_intent: ScheduledBaseIntent,
    ) -> ScheduledBaseIntentWrapper {
        let Some(committed_accounts) = base_intent.get_committed_accounts_mut()
        else {
            return ScheduledBaseIntentWrapper {
                inner: base_intent,
                excluded_pubkeys: Vec::new(),
                feepayers: Vec::new(),
                trigger_type: TriggerType::OnChain,
            };
        };

        struct Processor<'a> {
            excluded_pubkeys: HashSet<Pubkey>,
            feepayers: HashSet<FeePayerAccount>,
            bank: &'a Bank,
        }

        impl<'a> Processor<'a> {
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
                    todo!()
                }
                // TODO(edwin): hmm
                None => return true,
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

        ScheduledBaseIntentWrapper {
            inner: base_intent,
            feepayers: processor.feepayers.into_iter().collect(),
            excluded_pubkeys: processor.excluded_pubkeys.into_iter().collect(),
            trigger_type: TriggerType::OnChain,
        }
    }

    async fn result_processor(
        bank: Arc<Bank>,
        result_subscriber: oneshot::Receiver<
            broadcast::Receiver<BroadcastedIntentExecutionResult>,
        >,
        transaction_status_sender: TransactionStatusSender,
    ) {
        const SUBSCRIPTION_ERR_MSG: &str =
            "Failed to get subscription of results of BaseIntents execution";

        let mut result_receiver =
            result_subscriber.await.expect(SUBSCRIPTION_ERR_MSG);
        while let Ok(execution_result) = result_receiver.recv().await {
            match execution_result {
                Ok(value) => {
                    Self::process_intent_result(
                        &bank,
                        &transaction_status_sender,
                        value,
                    )
                    .await
                }
                Err(err) => {
                    error!("Failed to commit: {:?}", err);
                    todo!()
                }
            }
        }
    }

    async fn process_intent_result(
        bank: &Arc<Bank>,
        transaction_status_sender: &TransactionStatusSender,
        execution_outcome: ExecutionOutputWrapper,
    ) {
        // We don't trigger sent tx for `TriggerType::OffChain`
        // TODO: should be removed once crank supported
        if matches!(execution_outcome.trigger_type, TriggerType::OnChain) {
            register_scheduled_commit_sent(execution_outcome.sent_commit);
            match execute_legacy_transaction(
                execution_outcome.action_sent_transaction,
                bank,
                Some(transaction_status_sender),
            ) {
                Ok(signature) => debug!(
                    "Signaled sent commit with internal signature: {:?}",
                    signature
                ),
                Err(err) => {
                    error!(
                        "Failed to signal sent commit via transaction: {}",
                        err
                    );
                }
            }
        } else {
            info!(
                "OffChain triggered BaseIntent executed: {}",
                execution_outcome.sent_commit.message_id
            );
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

        let scheduled_base_intent_wrapped = scheduled_base_intent
            .into_iter()
            .map(|intent| self.preprocess_intent(intent))
            .collect();
        self.committor
            .commit_base_intent(scheduled_base_intent_wrapped);

        Ok(())
    }

    fn scheduled_commits_len(&self) -> usize {
        self.transaction_scheduler.scheduled_actions_len()
    }

    fn clear_scheduled_commits(&self) {
        self.transaction_scheduler.clear_scheduled_actions();
    }
}
