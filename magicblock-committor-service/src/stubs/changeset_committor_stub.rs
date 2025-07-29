use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::{SystemTime, UNIX_EPOCH},
};

use magicblock_committor_program::Changeset;
use magicblock_program::SentCommit;
use solana_pubkey::Pubkey;
use solana_sdk::{hash::Hash, signature::Signature, transaction::Transaction};
use tokio::sync::{oneshot, oneshot::Receiver};

use crate::{
    commit_scheduler::{
        BroadcastedMessageExecutionResult, ExecutionOutputWrapper,
    },
    error::CommittorServiceResult,
    message_executor::ExecutionOutput,
    persist::{
        CommitStatus, CommitStatusRow, CommitStatusSignatures, CommitStrategy,
        CommitType, L1MessagePersister, MessageSignatures,
    },
    service_ext::{L1MessageCommitorExtResult, L1MessageCommittorExt},
    types::{ScheduledL1MessageWrapper, TriggerType},
    utils::ScheduledMessageExt,
    L1MessageCommittor,
};

#[derive(Default)]
pub struct ChangesetCommittorStub {
    reserved_pubkeys_for_committee: Arc<Mutex<HashMap<Pubkey, Pubkey>>>,
    #[allow(clippy::type_complexity)]
    committed_changesets: Arc<Mutex<HashMap<u64, ScheduledL1MessageWrapper>>>,
}

impl L1MessageCommittor for ChangesetCommittorStub {
    fn reserve_pubkeys_for_committee(
        &self,
        committee: Pubkey,
        owner: Pubkey,
    ) -> Receiver<CommittorServiceResult<()>> {
        let (tx, rx) = oneshot::channel::<CommittorServiceResult<()>>();

        self.reserved_pubkeys_for_committee
            .lock()
            .unwrap()
            .insert(committee, owner);

        tx.send(Ok(())).unwrap_or_else(|_| {
            log::error!("Failed to send response");
        });
        rx
    }

    fn commit_l1_messages(&self, l1_messages: Vec<ScheduledL1MessageWrapper>) {
        let mut changesets = self.committed_changesets.lock().unwrap();
        l1_messages.into_iter().for_each(|message| {
            changesets.insert(message.scheduled_l1_message.id, message);
        });
    }

    fn subscribe_for_results(
        &self,
    ) -> Receiver<
        tokio::sync::broadcast::Receiver<BroadcastedMessageExecutionResult>,
    > {
        let (_, receiver) = oneshot::channel();
        receiver
    }

    fn get_commit_statuses(
        &self,
        message_id: u64,
    ) -> oneshot::Receiver<CommittorServiceResult<Vec<CommitStatusRow>>> {
        let (tx, rx) = oneshot::channel();

        let commit = self
            .committed_changesets
            .lock()
            .unwrap()
            .remove(&message_id);
        let Some(l1_message) = commit else {
            tx.send(Ok(vec![])).unwrap_or_else(|_| {
                log::error!("Failed to send commit status response");
            });
            return rx;
        };

        let status_rows = L1MessagePersister::create_commit_rows(
            &l1_message.scheduled_l1_message,
        );
        tx.send(Ok(status_rows)).unwrap_or_else(|_| {
            log::error!("Failed to send commit status response");
        });

        rx
    }

    fn get_commit_signatures(
        &self,
        commit_id: u64,
        pubkey: Pubkey,
    ) -> oneshot::Receiver<CommittorServiceResult<Option<MessageSignatures>>>
    {
        let (tx, rx) = oneshot::channel();
        let message_signature = MessageSignatures {
            processed_signature: Signature::new_unique(),
            finalized_signature: Some(Signature::new_unique()),
            created_at: now(),
        };

        tx.send(Ok(Some(message_signature))).unwrap_or_else(|_| {
            log::error!("Failed to send bundle signatures response");
        });

        rx
    }
}

#[async_trait::async_trait]
impl L1MessageCommittorExt for ChangesetCommittorStub {
    async fn commit_l1_messages_waiting(
        &self,
        l1_messages: Vec<ScheduledL1MessageWrapper>,
    ) -> L1MessageCommitorExtResult<Vec<BroadcastedMessageExecutionResult>>
    {
        let res = l1_messages
            .into_iter()
            .map(|message| {
                Ok(ExecutionOutputWrapper {
                    id: message.scheduled_l1_message.id,
                    output: ExecutionOutput {
                        commit_signature: Signature::new_unique(),
                        finalize_signature: Signature::new_unique(),
                    },
                    action_sent_transaction: Transaction::default(),
                    trigger_type: TriggerType::OnChain,
                    sent_commit: SentCommit {
                        message_id: message.scheduled_l1_message.id,
                        slot: message.scheduled_l1_message.slot,
                        blockhash: message.scheduled_l1_message.blockhash,
                        payer: message.scheduled_l1_message.payer,
                        requested_undelegation: message
                            .scheduled_l1_message
                            .is_undelegate(),
                        ..SentCommit::default()
                    },
                })
            })
            .collect::<Vec<_>>();

        Ok(res)
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("Time went backwards")
        .as_secs()
}
