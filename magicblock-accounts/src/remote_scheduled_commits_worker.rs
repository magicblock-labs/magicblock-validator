use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use log::{debug, error};
use magicblock_bank::bank::Bank;
use magicblock_committor_service::{
    persist::BundleSignatureRow, ChangesetMeta, L1MessageCommittor,
};
use magicblock_processor::execute_transaction::execute_legacy_transaction;
use magicblock_program::{
    magic_scheduled_l1_message::ScheduledL1Message,
    register_scheduled_commit_sent, SentCommit,
};
use magicblock_transaction_status::TransactionStatusSender;
use solana_sdk::transaction::Transaction;
use tokio::sync::mpsc::Receiver;

pub(crate) struct RemoteScheduledCommitsWorker<C: L1MessageCommittor> {
    bank: Arc<Bank>,
    committor: Arc<C>,
    transaction_status_sender: TransactionStatusSender,
    message_receiver: Receiver<Vec<ScheduledL1Message>>,
}

impl<C: L1MessageCommittor> RemoteScheduledCommitsWorker<C> {
    pub fn new(
        bank: Arc<Bank>,
        committor: Arc<C>,
        transaction_status_sender: TransactionStatusSender,
        message_receiver: Receiver<Vec<ScheduledL1Message>>,
    ) -> Self {
        Self {
            bank,
            committor,
            transaction_status_sender,
            message_receiver,
        }
    }

    pub async fn start(mut self) {
        while let Some(l1_messages) = self.message_receiver.recv().await {
            let metadata = ChangesetMeta::from(&l1_messages);
            // TODO(edwin) mayne actuall  self.committor.commit_l1_messages(l1_messages).
            // should be on a client, and here we just send receivers to wait on and process
            match self.committor.commit_l1_messages(l1_messages).await {
                Ok(Some(reqid)) => {
                    debug!(
                        "Committed changeset with {} accounts via reqid {}",
                        metadata.accounts.len(),
                        reqid
                    );
                }
                Ok(None) => {
                    debug!(
                        "Committed changeset with {} accounts, but did not get a reqid",
                        metadata.accounts.len()
                    );
                }
                Err(err) => {
                    error!(
                        "Tried to commit changeset with {} accounts but failed to send request ({:#?})",
                        metadata.accounts.len(),err
                    );
                }
            }

            self.process_message_result(metadata, todo!()).await;
        }
    }

    async fn process_message_result(
        &self,
        metadata: ChangesetMeta,
        mut sent_commits: HashMap<u64, (Transaction, SentCommit)>,
    ) {
        for bundle_id in metadata
            .accounts
            .iter()
            .map(|account| account.bundle_id)
            .collect::<HashSet<_>>()
        {
            let bundle_signatures = match self
                .committor
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
                    if let Some((commit_sent_transaction, mut sent_commit)) =
                        sent_commits.remove(&bundle_id)
                    {
                        sent_commit.chain_signatures = chain_signatures;
                        register_scheduled_commit_sent(sent_commit);
                        match execute_legacy_transaction(
                            commit_sent_transaction,
                            &self.bank,
                            Some(&self.transaction_status_sender),
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
    }
}
