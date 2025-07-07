use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use async_trait::async_trait;
use conjunto_transwise::AccountChainSnapshot;
use log::*;
use magicblock_account_cloner::{
    AccountClonerOutput, AccountClonerOutput::Cloned, CloneOutputMap,
};
use magicblock_accounts_api::InternalAccountProvider;
use magicblock_bank::bank::Bank;
use magicblock_committor_service::{
    persist::BundleSignatureRow, ChangedAccount, Changeset, ChangesetCommittor,
    ChangesetMeta,
};
use magicblock_processor::execute_transaction::execute_legacy_transaction;
use magicblock_program::{
    magic_scheduled_l1_message::ScheduledL1Message,
    register_scheduled_commit_sent, FeePayerAccount, Pubkey, ScheduledCommit,
    SentCommit, TransactionScheduler,
};
use magicblock_transaction_status::TransactionStatusSender;
use solana_sdk::{
    account::ReadableAccount, hash::Hash, transaction::Transaction,
};

use crate::{
    errors::AccountsResult, AccountCommittee, ScheduledCommitsProcessor,
};

pub struct RemoteScheduledCommitsProcessor {
    transaction_scheduler: TransactionScheduler,
    cloned_accounts: CloneOutputMap,
    bank: Arc<Bank>,
    transaction_status_sender: Option<TransactionStatusSender>,
}

#[async_trait]
impl ScheduledCommitsProcessor for RemoteScheduledCommitsProcessor {
    async fn process<IAP, CC>(
        &self,
        account_provider: &IAP,
        changeset_committor: &Arc<CC>,
    ) -> AccountsResult<()>
    where
        IAP: InternalAccountProvider,
        CC: ChangesetCommittor,
    {
        let scheduled_l1_messages =
            self.transaction_scheduler.take_scheduled_actions();

        if scheduled_l1_messages.is_empty() {
            return Ok(());
        }

        self.process_changeset(changeset_committor, changeset, sent_commits);

        Ok(())
    }

    fn scheduled_commits_len(&self) -> usize {
        self.transaction_scheduler.scheduled_actions_len()
    }

    fn clear_scheduled_commits(&self) {
        self.transaction_scheduler.clear_scheduled_actions();
    }
}

impl RemoteScheduledCommitsProcessor {
    pub fn new(
        bank: Arc<Bank>,
        cloned_accounts: CloneOutputMap,
        transaction_status_sender: Option<TransactionStatusSender>,
    ) -> Self {
        Self {
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

    fn process_changeset<CC: ChangesetCommittor>(
        &self,
        changeset_committor: &Arc<CC>,
        changeset: Changeset,
        mut sent_commits: HashMap<u64, (Transaction, SentCommit)>,
        ephemeral_blockhash: Hash,
    ) {
        // We process the changeset on a separate task in order to not block
        // the validator (slot advance) itself
        let changeset_committor = changeset_committor.clone();
        let bank = self.bank.clone();
        let transaction_status_sender = self.transaction_status_sender.clone();

        tokio::task::spawn(async move {
            // Create one sent commit transaction per bundle in our validator
            let changeset_metadata = ChangesetMeta::from(&changeset);
            debug!(
                "Committing changeset with {} accounts",
                changeset_metadata.accounts.len()
            );
            match changeset_committor
                .commit_changeset(changeset, ephemeral_blockhash, true)
                .await
            {
                Ok(Some(reqid)) => {
                    debug!(
                        "Committed changeset with {} accounts via reqid {}",
                        changeset_metadata.accounts.len(),
                        reqid
                    );
                }
                Ok(None) => {
                    debug!(
                        "Committed changeset with {} accounts, but did not get a reqid",
                        changeset_metadata.accounts.len()
                    );
                }
                Err(err) => {
                    error!(
                        "Tried to commit changeset with {} accounts but failed to send request ({:#?})",
                        changeset_metadata.accounts.len(),err
                    );
                }
            }
            for bundle_id in changeset_metadata
                .accounts
                .iter()
                .map(|account| account.bundle_id)
                .collect::<HashSet<_>>()
            {
                let bundle_signatures = match changeset_committor
                    .get_bundle_signatures(bundle_id)
                    .await
                {
                    Ok(Ok(sig)) => sig,
                    Ok(Err(err)) => {
                        error!("Encountered error while getting bundle signatures for {}: {:?}", bundle_id, err);
                        continue;
                    }
                    Err(err) => {
                        error!("Encountered error while getting bundle signatures for {}: {:?}", bundle_id, err);
                        continue;
                    }
                };
                match bundle_signatures {
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
