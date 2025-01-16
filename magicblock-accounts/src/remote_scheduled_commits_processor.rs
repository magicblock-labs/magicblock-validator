use std::{collections::HashSet, sync::Arc};

use async_trait::async_trait;
use conjunto_transwise::AccountChainSnapshot;
use log::*;
use magicblock_account_cloner::{
    AccountClonerOutput, AccountClonerOutput::Cloned, CloneOutputMap,
};
use magicblock_accounts_api::InternalAccountProvider;
use magicblock_bank::bank::Bank;
use magicblock_core::debug_panic;
use magicblock_metrics::metrics;
use magicblock_mutator::Cluster;
use magicblock_processor::execute_transaction::execute_legacy_transaction;
use magicblock_program::{
    register_scheduled_commit_sent, FeePayerAccount, SentCommit,
    TransactionScheduler,
};
use magicblock_transaction_status::TransactionStatusSender;
use solana_sdk::{pubkey::Pubkey, signature::Signature};

use crate::{
    errors::{AccountsError, AccountsResult},
    remote_account_committer::update_account_commit_metrics,
    AccountCommittee, AccountCommitter, ScheduledCommitsProcessor,
    SendableCommitAccountsPayload,
};

pub struct RemoteScheduledCommitsProcessor {
    #[allow(unused)]
    cluster: Cluster,
    bank: Arc<Bank>,
    transaction_status_sender: Option<TransactionStatusSender>,
    transaction_scheduler: TransactionScheduler,
    cloned_accounts: CloneOutputMap,
}

#[async_trait]
impl ScheduledCommitsProcessor for RemoteScheduledCommitsProcessor {
    async fn process<AC, IAP>(
        &self,
        committer: &Arc<AC>,
        account_provider: &IAP,
    ) -> AccountsResult<()>
    where
        AC: AccountCommitter,
        IAP: InternalAccountProvider,
    {
        let scheduled_commits =
            self.transaction_scheduler.take_scheduled_commits();

        if scheduled_commits.is_empty() {
            return Ok(());
        }

        let mut sendable_payloads_queue = vec![];
        for commit in scheduled_commits {
            info!("Processing commit: {:?}", commit);

            // Determine which accounts are available and can be committed
            let mut committees = vec![];
            let all_pubkeys: HashSet<Pubkey> = HashSet::from_iter(
                commit
                    .accounts
                    .iter()
                    .map(|ca| ca.pubkey)
                    .collect::<Vec<_>>(),
            );
            let mut feepayers = HashSet::new();

            for committed_account in commit.accounts {
                let mut commitment_pubkey = committed_account.pubkey;
                let mut commitment_pubkey_owner = committed_account.owner;
                if let Some(Cloned {
                    account_chain_snapshot,
                    ..
                }) = Self::fetch_cloned_account(
                    &committed_account.pubkey,
                    &self.cloned_accounts,
                ) {
                    // If the account is a FeePayer, we committed the mapped delegated account
                    if account_chain_snapshot.chain_state.is_feepayer() {
                        commitment_pubkey =
                            AccountChainSnapshot::ephemeral_balance_pda(
                                &committed_account.pubkey,
                            );
                        commitment_pubkey_owner =
                            AccountChainSnapshot::ephemeral_balance_pda_owner();
                        feepayers.insert(FeePayerAccount {
                            pubkey: committed_account.pubkey,
                            delegated_pda: commitment_pubkey,
                        });
                    } else if account_chain_snapshot
                        .chain_state
                        .is_undelegated()
                    {
                        error!("Scheduled commit account '{}' is undelegated. This is not supported.", committed_account.pubkey);
                    }
                }

                match account_provider.get_account(&committed_account.pubkey) {
                    Some(account_data) => {
                        committees.push(AccountCommittee {
                            pubkey: commitment_pubkey,
                            owner: commitment_pubkey_owner,
                            account_data,
                            slot: commit.slot,
                            undelegation_requested: commit.request_undelegation,
                        });
                    }
                    None => {
                        error!(
                            "Scheduled commmit account '{}' not found. It must have gotten undelegated and removed since it was scheduled.",
                            committed_account.pubkey
                        );
                    }
                }
            }

            let payloads = vec![
                committer
                    .create_commit_accounts_transaction(committees)
                    .await?,
            ];

            // Determine which payloads are a noop since all accounts are up to date
            // and which require a commit to chain
            let mut included_pubkeys = HashSet::new();
            let sendable_payloads = payloads
                .into_iter()
                .filter_map(|payload| {
                    if let Some(transaction) = payload.transaction {
                        included_pubkeys.extend(
                            payload
                                .committees
                                .iter()
                                .map(|(pubkey, _)| *pubkey),
                        );
                        Some(SendableCommitAccountsPayload {
                            transaction,
                            committees: payload.committees,
                        })
                    } else {
                        None
                    }
                })
                .collect::<Vec<SendableCommitAccountsPayload>>();

            // Tally up the pubkeys that will not be committed since the account
            // was not available as determined when creating sendable payloads
            let excluded_pubkeys = all_pubkeys
                .into_iter()
                .filter(|pubkey| {
                    !included_pubkeys.contains(pubkey)
                        && !included_pubkeys.contains(
                            &AccountChainSnapshot::ephemeral_balance_pda(
                                pubkey,
                            ),
                        )
                })
                .collect::<Vec<Pubkey>>();

            // Extract signatures of all transactions that we will execute on
            // chain in order to realize the commits needed
            let signatures = sendable_payloads
                .iter()
                .map(|payload| payload.get_signature())
                .collect::<Vec<Signature>>();

            // Record that we are about to send the commit to chain including all
            // information (mainly signatures) needed to track its outcome on chain
            let sent_commit = SentCommit {
                commit_id: commit.id,
                slot: commit.slot,
                blockhash: commit.blockhash,
                payer: commit.payer,
                chain_signatures: signatures,
                included_pubkeys: included_pubkeys.into_iter().collect(),
                excluded_pubkeys,
                feepayers,
                requested_undelegation: commit.request_undelegation,
            };
            register_scheduled_commit_sent(sent_commit);
            let signature = execute_legacy_transaction(
                commit.commit_sent_transaction,
                &self.bank,
                self.transaction_status_sender.as_ref(),
            )
            .map_err(Box::new)?;

            // In the case that no account needs to be committed we record that in
            // our ledger and are done
            if sendable_payloads.is_empty() {
                debug!(
                    "Signaled no commit needed with internal signature: {:?}",
                    signature
                );
                continue;
            } else {
                debug!(
                    "Signaled commit with internal signature: {:?}",
                    signature
                );
            }

            // Queue up the actual commit
            sendable_payloads_queue.extend(sendable_payloads);
        }

        self.process_accounts_commits_in_background(
            committer,
            sendable_payloads_queue,
        );

        Ok(())
    }

    fn scheduled_commits_len(&self) -> usize {
        self.transaction_scheduler.scheduled_commits_len()
    }

    fn clear_scheduled_commits(&self) {
        self.transaction_scheduler.clear_scheduled_commits();
    }
}

impl RemoteScheduledCommitsProcessor {
    pub(crate) fn new(
        cluster: Cluster,
        bank: Arc<Bank>,
        cloned_accounts: CloneOutputMap,
        transaction_status_sender: Option<TransactionStatusSender>,
    ) -> Self {
        Self {
            cluster,
            bank,
            transaction_status_sender,
            cloned_accounts,
            transaction_scheduler: TransactionScheduler::default(),
        }
    }

    fn process_accounts_commits_in_background<AC: AccountCommitter>(
        &self,
        committer: &Arc<AC>,
        sendable_payloads_queue: Vec<SendableCommitAccountsPayload>,
    ) {
        // We process the queue on a separate task in order to not block
        // the validator (slot advance) itself
        // NOTE: @@ we have to be careful here and ensure that the validator does not
        // shutdown before this task is done
        // We will need some tracking machinery which is overkill until we get to the
        // point where we do allow validator shutdown
        let committer = committer.clone();
        tokio::task::spawn(async move {
            let pending_commits = match committer
                .send_commit_transactions(sendable_payloads_queue)
                .await
            {
                Ok(pending) => pending,
                Err(AccountsError::FailedToSendCommitTransaction(
                    err,
                    commit_and_undelegate_accounts,
                    commit_only_accounts,
                )) => {
                    update_account_commit_metrics(
                        &commit_and_undelegate_accounts,
                        &commit_only_accounts,
                        metrics::Outcome::Error,
                        None,
                    );
                    debug_panic!(
                        "Failed to send commit transactions: {:?}",
                        err
                    );
                    return;
                }
                Err(err) => {
                    debug_panic!(
                        "Failed to send commit transactions, received invalid err: {:?}",
                        err
                    );
                    return;
                }
            };

            committer.confirm_pending_commits(pending_commits).await;
        });
    }

    fn fetch_cloned_account(
        pubkey: &Pubkey,
        cloned_accounts: &CloneOutputMap,
    ) -> Option<AccountClonerOutput> {
        cloned_accounts
            .read()
            .expect("RwLock of RemoteAccountClonerWorker.last_clone_output is poisoned")
            .get(pubkey).cloned()
    }
}
