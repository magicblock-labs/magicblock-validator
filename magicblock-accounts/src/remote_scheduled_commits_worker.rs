use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use log::{debug, error};
use magicblock_bank::bank::Bank;
use magicblock_committor_service::{
    commit_scheduler::{
        BroadcastedMessageExecutionResult, ExecutionOutputWrapper,
    },
    l1_message_executor::ExecutionOutput,
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
        while let Ok(execution_result) = result_receiver.recv().await {
            match execution_result {
                Ok(value) => self.process_message_result(value).await,
                Err(err) => {
                    todo!()
                }
            }
        }
    }

    async fn process_message_result(
        &self,
        execution_outcome: ExecutionOutputWrapper,
    ) {
        register_scheduled_commit_sent(execution_outcome.sent_commit);
        match execute_legacy_transaction(
            execution_outcome.action_sent_transaction,
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
}
