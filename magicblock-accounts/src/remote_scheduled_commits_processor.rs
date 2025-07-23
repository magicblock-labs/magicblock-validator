use std::{collections::HashSet, sync::Arc};

use async_trait::async_trait;
use conjunto_transwise::AccountChainSnapshot;
use log::error;
use magicblock_account_cloner::{AccountClonerOutput, CloneOutputMap};
use magicblock_bank::bank::Bank;
use magicblock_committor_service::{
    types::ScheduledL1MessageWrapper, utils::ScheduledMessageExt,
    L1MessageCommittor,
};
use magicblock_program::{
    magic_scheduled_l1_message::{CommittedAccountV2, ScheduledL1Message},
    FeePayerAccount, TransactionScheduler,
};
use magicblock_transaction_status::TransactionStatusSender;
use solana_sdk::{
    account::{Account, ReadableAccount},
    pubkey::Pubkey,
};
use tokio::sync::mpsc::{channel, Sender};

use crate::{
    errors::AccountsResult,
    remote_scheduled_commits_worker::RemoteScheduledCommitsWorker,
    ScheduledCommitsProcessor,
};

const POISONED_RWLOCK_MSG: &str =
    "RwLock of RemoteAccountClonerWorker.last_clone_output is poisoned";

pub struct RemoteScheduledCommitsProcessor<C: L1MessageCommittor> {
    transaction_scheduler: TransactionScheduler,
    cloned_accounts: CloneOutputMap,
    bank: Arc<Bank>,
    committor: Arc<C>,
}

impl<C: L1MessageCommittor> RemoteScheduledCommitsProcessor<C> {
    pub fn new(
        bank: Arc<Bank>,
        cloned_accounts: CloneOutputMap,
        committor: Arc<C>,
        transaction_status_sender: TransactionStatusSender,
    ) -> Self {
        let result_subscriber = committor.subscribe_for_results();
        let worker = RemoteScheduledCommitsWorker::new(
            bank.clone(),
            result_subscriber,
            transaction_status_sender,
        );
        tokio::spawn(worker.start());

        Self {
            bank,
            cloned_accounts,
            committor,
            transaction_scheduler: TransactionScheduler::default(),
        }
    }

    // fn preprocess_messages(&self, mut l1_messages: Vec<ScheduledL1Message>) -> Vec<ScheduledL1MessageWrapper> {
    //     l1_messages.
    // }

    fn preprocess_message(
        &self,
        mut l1_message: ScheduledL1Message,
    ) -> ScheduledL1MessageWrapper {
        let Some(committed_accounts) = l1_message.get_committed_accounts_mut()
        else {
            return ScheduledL1MessageWrapper {
                scheduled_l1_message: l1_message,
                excluded_pubkeys: Vec::new(),
                feepayers: Vec::new(),
            };
        };

        let mut excluded_pubkeys = HashSet::new();
        let mut feepayers = HashSet::new();

        let process_feepayer = |account: &mut CommittedAccountV2| -> bool {
            let pubkey = account.pubkey;
            let ephemeral_pubkey =
                AccountChainSnapshot::ephemeral_balance_pda(&pubkey);

            feepayers.insert(FeePayerAccount {
                pubkey: *pubkey,
                delegated_pda: ephemeral_pubkey,
            });

            match self.bank.get_account(&ephemeral_pubkey) {
                Some(account_data) => {
                    let ephemeral_owner =
                        AccountChainSnapshot::ephemeral_balance_pda_owner();
                    account.pubkey = ephemeral_pubkey;
                    account.account = Account {
                        lamports: account_data.lamports(),
                        data: account_data.data().to_vec(),
                        owner: ephemeral_owner,
                        executable: account_data.executable(),
                        rent_epoch: account_data.rent_epoch(),
                    };
                    true
                }
                None => {
                    error!(
                    "Scheduled commit account '{}' not found. It must have gotten undelegated and removed since it was scheduled.",
                    pubkey
                );
                    excluded_pubkeys.insert(*pubkey);
                    false
                }
            }
        };

        committed_accounts.retain_mut(|account| {
            let pubkey = account.pubkey;
            let cloned_accounts =
                self.cloned_accounts.read().expect(POISONED_RWLOCK_MSG);

            match cloned_accounts.get(&pubkey) {
                Some(AccountClonerOutput::Cloned {
                    account_chain_snapshot,
                    ..
                }) => {
                    if account_chain_snapshot.chain_state.is_feepayer() {
                        process_feepayer(account)
                    } else if account_chain_snapshot
                        .chain_state
                        .is_undelegated()
                    {
                        excluded_pubkeys.insert(pubkey);
                        false
                    } else {
                        true
                    }
                }
                Some(AccountClonerOutput::Unclonable {..}) => {
                    todo!()
                }
                None => true,
            }
        });

        ScheduledL1MessageWrapper {
            scheduled_l1_message: l1_message,
            feepayers: feepayers.into_iter().collect(),
            excluded_pubkeys: excluded_pubkeys.into_iter().collect(),
        }
    }

    fn preprocess_message2(
        &self,
        mut l1_message: ScheduledL1Message,
    ) -> ScheduledL1MessageWrapper {
        let Some(committed_accounts) = l1_message.get_committed_accounts_mut()
        else {
            return ScheduledL1MessageWrapper {
                scheduled_l1_message: l1_message,
                excluded_pubkeys: vec![],
                feepayers: vec![],
            };
        };

        let mut excluded_pubkeys = HashSet::new();
        let mut feepayers = HashSet::new();
        committed_accounts.retain_mut(|account| {
            let pubkey = account.pubkey;
            if let Some(AccountClonerOutput::Cloned {
                account_chain_snapshot,
                ..
            }) = self
                .cloned_accounts
                .read()
                .expect(POISONED_RWLOCK_MSG)
                .get(&pubkey)
            {
                if account_chain_snapshot.chain_state.is_feepayer() {
                    let ephemeral_pubkey =
                        AccountChainSnapshot::ephemeral_balance_pda(&pubkey);
                    let ephemeral_owner =
                        AccountChainSnapshot::ephemeral_balance_pda_owner();
                    feepayers.insert(FeePayerAccount {
                        pubkey: *pubkey,
                        delegated_pda: ephemeral_pubkey,
                    });

                    if let Some(account_data) = self.bank.get_account(&ephemeral_pubkey) {
                        account.pubkey = ephemeral_pubkey;
                        account.account = Account {
                            lamports: account_data.lamports(),
                            data: account_data.data().to_vec(),
                            owner: ephemeral_owner,
                            executable: account_data.executable(),
                            rent_epoch: account_data.rent_epoch()
                        };
                        true
                    } else {
                        error!(
                            "Scheduled commmit account '{}' not found. It must have gotten undelegated and removed since it was scheduled.",
                            pubkey
                        );

                        // Exclude this
                        // TODO(edwin): should fail commit really
                        excluded_pubkeys.insert(pubkey);
                        false
                    }
                } else if account_chain_snapshot.chain_state.is_undelegated() {
                    excluded_pubkeys.insert(pubkey);
                    false
                } else {
                    true
                }
            } else {
                true
            }
        });

        ScheduledL1MessageWrapper {
            scheduled_l1_message: l1_message,
            feepayers: feepayers.into_iter().collect(),
            excluded_pubkeys: excluded_pubkeys.into_iter().collect(),
        }
    }
}

#[async_trait]
impl<C: L1MessageCommittor> ScheduledCommitsProcessor
    for RemoteScheduledCommitsProcessor<C>
{
    async fn process(&self) -> AccountsResult<()> {
        let scheduled_l1_messages =
            self.transaction_scheduler.take_scheduled_actions();

        if scheduled_l1_messages.is_empty() {
            return Ok(());
        }

        let l1_messages = self.preprocess(scheduled_l1_messages);
        self.committor.commit_l1_messages(scheduled_l1_messages);

        Ok(())
    }

    fn scheduled_commits_len(&self) -> usize {
        self.transaction_scheduler.scheduled_actions_len()
    }

    fn clear_scheduled_commits(&self) {
        self.transaction_scheduler.clear_scheduled_actions();
    }
}
