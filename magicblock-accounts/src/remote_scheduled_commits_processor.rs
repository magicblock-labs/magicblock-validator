use async_trait::async_trait;
use conjunto_transwise::AccountChainSnapshot;
use log::*;
use magicblock_bank::bank::Bank;
use magicblock_processor::execute_transaction::execute_legacy_transaction;
use magicblock_transaction_status::TransactionStatusSender;
use solana_sdk::{account::ReadableAccount, transaction::Transaction};
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use magicblock_account_cloner::{
    AccountClonerOutput, AccountClonerOutput::Cloned, CloneOutputMap,
};
use magicblock_accounts_api::InternalAccountProvider;
use magicblock_committor_service::{
    persist::BundleSignatureRow, ChangedAccount, Changeset, ChangesetMeta,
    CommittorService,
};
use magicblock_program::{
    register_scheduled_commit_sent, FeePayerAccount, Pubkey, SentCommit,
    TransactionScheduler,
};

use crate::{
    errors::AccountsResult, AccountCommittee, ScheduledCommitsProcessor,
};

pub struct RemoteScheduledCommitsProcessor {
    committor_service: Arc<CommittorService>,
    transaction_scheduler: TransactionScheduler,
    cloned_accounts: CloneOutputMap,
    bank: Arc<Bank>,
    transaction_status_sender: Option<TransactionStatusSender>,
}

#[async_trait]
impl ScheduledCommitsProcessor for RemoteScheduledCommitsProcessor {
    async fn process<IAP>(&self, account_provider: &IAP) -> AccountsResult<()>
    where
        IAP: InternalAccountProvider,
    {
        let scheduled_commits =
            self.transaction_scheduler.take_scheduled_commits();

        if scheduled_commits.is_empty() {
            return Ok(());
        }

        let mut changeset = Changeset::default();
        // SAFETY: we only get here if the scheduled commits are not empty
        let max_slot = scheduled_commits
            .iter()
            .map(|commit| commit.slot)
            .max()
            .unwrap();

        changeset.slot = max_slot;

        let mut sent_commits = HashMap::new();
        for commit in scheduled_commits {
            // Determine which accounts are available and can be committed
            let mut committees = vec![];
            let mut feepayers = HashSet::new();
            let mut excluded_pubkeys = vec![];
            for committed_account in commit.accounts {
                let mut committee_pubkey = committed_account.pubkey;
                let mut committee_owner = committed_account.owner;
                if let Some(Cloned {
                    account_chain_snapshot,
                    ..
                }) = Self::fetch_cloned_account(
                    &committed_account.pubkey,
                    &self.cloned_accounts,
                ) {
                    // If the account is a FeePayer, we commit the mapped delegated account
                    if account_chain_snapshot.chain_state.is_feepayer() {
                        committee_pubkey =
                            AccountChainSnapshot::ephemeral_balance_pda(
                                &committed_account.pubkey,
                            );
                        committee_owner =
                            AccountChainSnapshot::ephemeral_balance_pda_owner();
                        feepayers.insert(FeePayerAccount {
                            pubkey: committed_account.pubkey,
                            delegated_pda: committee_pubkey,
                        });
                    } else if account_chain_snapshot
                        .chain_state
                        .is_undelegated()
                    {
                        error!("Scheduled commit account '{}' is undelegated. This is not supported.", committed_account.pubkey);
                        excluded_pubkeys.push(committed_account.pubkey);
                        continue;
                    }
                }

                match account_provider.get_account(&committed_account.pubkey) {
                    Some(account_data) => {
                        committees.push((
                            commit.id,
                            AccountCommittee {
                                pubkey: committee_pubkey,
                                owner: committee_owner,
                                account_data,
                                slot: commit.slot,
                                undelegation_requested: commit
                                    .request_undelegation,
                            },
                        ));
                    }
                    None => {
                        error!(
                            "Scheduled commmit account '{}' not found. It must have gotten undelegated and removed since it was scheduled.",
                            committed_account.pubkey
                        );
                        excluded_pubkeys.push(committed_account.pubkey);
                        continue;
                    }
                }
            }

            // Collect all SentCommit info available at this stage
            // We add the chain_signatures after we sent off the changeset
            let sent_commit = SentCommit {
                commit_id: commit.id,
                payer: commit.payer,
                blockhash: commit.blockhash,
                included_pubkeys: committees
                    .iter()
                    .map(|(_, committee)| committee.pubkey)
                    .collect(),
                excluded_pubkeys,
                feepayers,
                requested_undelegation: commit.request_undelegation,
                ..Default::default()
            };
            sent_commits.insert(
                commit.id,
                (commit.commit_sent_transaction, sent_commit),
            );

            // Add the committee to the changeset
            for (bundle_id, committee) in committees {
                changeset.add(
                    committee.pubkey,
                    ChangedAccount::Full {
                        lamports: committee.account_data.lamports(),
                        data: committee.account_data.data().to_vec(),
                        owner: committee.owner,
                        bundle_id,
                    },
                );
            }
        }

        self.process_changeset(changeset, sent_commits);

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
    pub fn new(
        committer_service: Arc<CommittorService>,
        bank: Arc<Bank>,
        cloned_accounts: CloneOutputMap,
        transaction_status_sender: Option<TransactionStatusSender>,
    ) -> Self {
        Self {
            committor_service: committer_service,
            bank,
            transaction_status_sender,
            cloned_accounts,
            transaction_scheduler: TransactionScheduler::default(),
        }
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

    fn process_changeset(
        &self,
        changeset: Changeset,
        mut sent_commits: HashMap<u64, (Transaction, SentCommit)>,
    ) {
        // We process the changeset on a separate task in order to not block
        // the validator (slot advance) itself
        let committor_service = self.committor_service.clone();
        let bank = self.bank.clone();
        let transaction_status_sender = self.transaction_status_sender.clone();

        tokio::task::spawn(async move {
            // Create one sent commit transaction per bundle in our validator
            let changeset_metadata = ChangesetMeta::from(&changeset);
            for bundle_id in changeset_metadata
                .accounts
                .iter()
                .map(|account| account.bundle_id)
                .collect::<HashSet<_>>()
            {
                match committor_service
                    .get_bundle_signatures(bundle_id)
                    .await
                    // TODO: @@@
                    .unwrap()
                    .unwrap()
                {
                    Some(BundleSignatureRow {
                        processed_signature,
                        finalized_signature,
                        bundle_id,
                        ..
                    }) => {
                        let mut chain_signatures = vec![processed_signature];
                        if let Some(finalized_signature) = finalized_signature {
                            chain_signatures.push(finalized_signature);
                        }
                        if let Some((
                            commit_sent_transaction,
                            mut sent_commit,
                        )) = sent_commits.remove(&bundle_id)
                        {
                            sent_commit.chain_signatures = chain_signatures;
                            register_scheduled_commit_sent(sent_commit);
                            match execute_legacy_transaction(
                                commit_sent_transaction,
                                &bank,
                                transaction_status_sender.as_ref()
                            ) {
                            Ok(signature) => debug!(
                                "Signaled sent commit with internal signature: {:?}",
                                signature
                            ),
                            Err(err) => {
                                error!("Failed to signal sent commit via transaction: {}", err);
                            }
                        }
                        } else {
                            error!(
                                "BUG: Failed to get sent commit for bundle id {} that should have been added",
                                bundle_id
                            );
                        }
                    }
                    None => error!(
                        "Failed to get bundle signatures for bundle id {}",
                        bundle_id
                    ),
                }
            }
        });
    }
}
