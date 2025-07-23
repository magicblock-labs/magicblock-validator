use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use log::{debug, error};
use magicblock_bank::bank::Bank;
use magicblock_committor_service::{
    l1_message_executor::{BroadcastedMessageExecutionResult, ExecutionOutput},
    persist::MessageSignatures,
    ChangesetMeta, L1MessageCommittor,
};
use magicblock_processor::execute_transaction::execute_legacy_transaction;
use magicblock_program::{
    magic_scheduled_l1_message::ScheduledL1Message,
    register_scheduled_commit_sent, SentCommit,
};
use magicblock_transaction_status::TransactionStatusSender;
use solana_sdk::transaction::Transaction;
use tokio::sync::{broadcast, mpsc::Receiver, oneshot};

pub(crate) struct RemoteScheduledCommitsWorker<C: L1MessageCommittor> {
    bank: Arc<Bank>,
    result_subscriber: oneshot::Receiver<
        broadcast::Receiver<BroadcastedMessageExecutionResult>,
    >,
    transaction_status_sender: TransactionStatusSender,
}

impl<C: L1MessageCommittor> RemoteScheduledCommitsWorker<C> {
    pub fn new(
        bank: Arc<Bank>,
        result_subscriber: oneshot::Receiver<
            broadcast::Receiver<BroadcastedMessageExecutionResult>,
        >,
        transaction_status_sender: TransactionStatusSender,
    ) -> Self {
        Self {
            bank,
            result_subscriber,
            transaction_status_sender,
        }
    }

    // TODO(edwin): maybe not needed
    pub async fn start(mut self) {
        const SUBSCRIPTION_ERR_MSG: &str =
            "Failed to get subscription of results of L1Messages execution";

        let mut result_receiver =
            self.result_subscriber.await.expect(SUBSCRIPTION_ERR_MSG);
        while let Ok(l1_messages) = result_receiver.recv().await {
            let metadata = ChangesetMeta::from(&l1_messages);
            self.process_message_result(metadata, todo!()).await;
        }
    }

    async fn process_message_result(&self, execution_outcome: ExecutionOutput) {
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
    }

    async fn process_message_result_old(
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
                .get_commit_signatures(bundle_id)
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
                Some(MessageSignatures {
                    processed_signature,
                    finalized_signature,
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
