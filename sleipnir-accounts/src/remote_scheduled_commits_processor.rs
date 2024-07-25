use std::{collections::HashSet, sync::Arc};

use async_trait::async_trait;
use log::*;
use sleipnir_bank::bank::Bank;
use sleipnir_mutator::Cluster;
use sleipnir_program::{
    register_scheduled_commit_sent, SentCommit, TransactionScheduler,
};
use sleipnir_transaction_status::TransactionStatusSender;
use solana_sdk::{pubkey::Pubkey, signature::Signature};

use crate::{
    errors::AccountsResult, utils::accounts_execute_transaction,
    AccountCommittee, AccountCommitter, InternalAccountProvider,
    ScheduledCommitsProcessor, SendableCommitAccountsPayload,
};

pub struct RemoteScheduledCommitsProcessor {
    #[allow(unused)]
    cluster: Cluster,
    bank: Arc<Bank>,
    transaction_status_sender: Option<TransactionStatusSender>,
    transaction_scheduler: TransactionScheduler,
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
}

#[async_trait]
impl ScheduledCommitsProcessor for RemoteScheduledCommitsProcessor {
    async fn process<AC: AccountCommitter, IAP: InternalAccountProvider>(
        &self,
        committer: &Arc<AC>,
        account_provider: &IAP,
    ) -> AccountsResult<()> {
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
                        committees.push(AccountCommittee {
                            pubkey,
                            account_data,
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

            // NOTE: @@@ if chain cannot be reached we error when getting blockhash
            // and should still log this info to our ledger
            let payloads = committer
                .create_commit_accounts_transactions(committees, None)
                .await?;

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
            // was not available or still up to date as determined when creating
            // sendable payloads
            let excluded_pubkeys = all_pubkeys
                .into_iter()
                .filter(|pubkey| !included_pubkeys.contains(pubkey))
                .collect::<Vec<Pubkey>>();

            // Extract signatures of all transactions that we we will execute on
            // chain in order to realize the commits needed
            let signatures = sendable_payloads
                .iter()
                .flat_map(|payload| payload.transaction.signatures.clone())
                .collect::<Vec<Signature>>();

            // Record that we are about to send the commit to chain including all
            // information (mainly signatures) needed to track its outcome on chain
            let sent_commit = SentCommit::new(
                commit.id,
                commit.slot,
                commit.blockhash,
                commit.payer,
                signatures,
                included_pubkeys.into_iter().collect(),
                excluded_pubkeys,
            );
            register_scheduled_commit_sent(sent_commit);
            let signature = accounts_execute_transaction(
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

        // Finally we process the queue on a separate task in order to not block
        // the validator (slot advance) itself
        // NOTE: @@ we have to be careful here and ensure that the validator does not
        // shutdown before this task is done
        // We will need some tracking machinery which is overkill until we get to the
        // point where we do allow validator shutdown
        let committer = committer.clone();
        tokio::task::spawn(async move {
            let signatures = committer
                .send_commit_transactions(sendable_payloads_queue)
                .await;
            debug!(
                "Signaled commit with external signatures: {:?}",
                signatures
            );
        });

        Ok(())
    }
}

// TODO: @@@ tests
