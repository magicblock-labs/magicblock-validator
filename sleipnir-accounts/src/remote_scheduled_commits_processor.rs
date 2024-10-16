use std::{collections::HashSet, sync::Arc};

use async_trait::async_trait;
use log::*;
use sleipnir_accounts_api::InternalAccountProvider;
use sleipnir_bank::bank::Bank;
use sleipnir_core::debug_panic;
use sleipnir_metrics::metrics;
use sleipnir_mutator::Cluster;
use sleipnir_processor::execute_transaction::execute_legacy_transaction;
use sleipnir_program::{
    register_scheduled_commit_sent, SentCommit, TransactionScheduler,
};
use sleipnir_transaction_status::TransactionStatusSender;
use solana_sdk::{pubkey::Pubkey, signature::Signature};

use crate::{
    errors::AccountsResult, AccountCommittee, AccountCommitter,
    ScheduledCommitsProcessor, SendableCommitAccountsPayload,
    UndelegationRequest,
};

pub struct RemoteScheduledCommitsProcessor {
    #[allow(unused)]
    cluster: Cluster,
    bank: Arc<Bank>,
    transaction_status_sender: Option<TransactionStatusSender>,
    transaction_scheduler: TransactionScheduler,
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
            let all_pubkeys: HashSet<Pubkey> =
                HashSet::from_iter(commit.accounts.iter().cloned());

            for pubkey in commit.accounts {
                match account_provider.get_account(&pubkey) {
                    Some(account_data) => {
                        let undelegation_request =
                            if commit.request_undelegation {
                                Some(UndelegationRequest {
                                    owner: commit.owner,
                                })
                            } else {
                                None
                            };
                        committees.push(AccountCommittee {
                            pubkey,
                            account_data,
                            slot: commit.slot,
                            undelegation_request,
                        });
                    }
                    None => {
                        error!(
                            "Scheduled commmit account '{}' not found. It must have gotten undelegated and removed since it was scheduled.",
                            pubkey
                        );
                    }
                }
            }

            // NOTE: when we address https://github.com/magicblock-labs/magicblock-validator/issues/100
            // we should report if we cannot get the blockhash as part of the _sent commit_
            // transaction
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
                .filter(|pubkey| !included_pubkeys.contains(pubkey))
                .collect::<Vec<Pubkey>>();

            // Extract signatures of all transactions that we we will execute on
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
                requested_undelegation_to_owner: commit
                    .request_undelegation
                    .then_some(commit.owner),
            };
            register_scheduled_commit_sent(sent_commit);
            let signature = execute_legacy_transaction(
                commit.commit_sent_transaction,
                &self.bank,
                self.transaction_status_sender.as_ref(),
            )?;

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
}

impl RemoteScheduledCommitsProcessor {
    pub(crate) fn new(
        cluster: Cluster,
        bank: Arc<Bank>,
        transaction_status_sender: Option<TransactionStatusSender>,
    ) -> Self {
        Self {
            cluster,
            bank,
            transaction_status_sender,
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
            let (commit_only_accounts, commit_and_undelegate_accounts) =
                sendable_payloads_queue.iter().fold(
                    (HashSet::new(), HashSet::new()),
                    |(mut commit_only, mut undelegated), commit| {
                        for (pubkey, _) in &commit.committees {
                            if commit
                                .transaction
                                .undelegated_accounts
                                .contains(pubkey)
                            {
                                undelegated.insert(*pubkey);
                            } else {
                                commit_only.insert(*pubkey);
                            }
                        }
                        (commit_only, undelegated)
                    },
                );

            match committer
                .send_commit_transactions(sendable_payloads_queue)
                .await
            {
                Ok(commits) => commits,
                Err(err) => {
                    debug_panic!(
                        "Failed to send commit transactions: {:?}",
                        err
                    );
                    return;
                }
            };

            if !commit_and_undelegate_accounts.is_empty()
                && log_enabled!(log::Level::Debug)
            {
                debug!(
                    "Requesting to undelegate: {}",
                    commit_and_undelegate_accounts
                        .iter()
                        .map(|x| x.to_string())
                        .collect::<Vec<String>>()
                        .join(", ")
                );
            }
            for pubkey in commit_and_undelegate_accounts {
                metrics::inc_account_commit(
                    metrics::AccountCommit::CommitAndUndelegate {
                        pubkey: &pubkey.to_string(),
                    },
                );
            }
            for pubkey in commit_only_accounts {
                metrics::inc_account_commit(
                    metrics::AccountCommit::CommitOnly {
                        pubkey: &pubkey.to_string(),
                    },
                );
            }
        });
    }
}
