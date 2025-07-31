use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

use magicblock_program::SentCommit;
use solana_pubkey::Pubkey;
use solana_sdk::{signature::Signature, transaction::Transaction};
use tokio::sync::{oneshot, oneshot::Receiver};

use crate::{
    error::CommittorServiceResult,
    intent_execution_manager::{
        BroadcastedIntentExecutionResult, ExecutionOutputWrapper,
    },
    intent_executor::ExecutionOutput,
    persist::{CommitStatusRow, IntentPersisterImpl, MessageSignatures},
    service_ext::{BaseIntentCommitorExtResult, BaseIntentCommittorExt},
    types::{ScheduledBaseIntentWrapper, TriggerType},
    BaseIntentCommittor,
};

#[derive(Default)]
pub struct ChangesetCommittorStub {
    reserved_pubkeys_for_committee: Arc<Mutex<HashMap<Pubkey, Pubkey>>>,
    #[allow(clippy::type_complexity)]
    committed_changesets: Arc<Mutex<HashMap<u64, ScheduledBaseIntentWrapper>>>,
}

impl ChangesetCommittorStub {
    pub fn len(&self) -> usize {
        self.committed_changesets.lock().unwrap().len()
    }
}

impl BaseIntentCommittor for ChangesetCommittorStub {
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

    fn commit_base_intent(
        &self,
        base_intents: Vec<ScheduledBaseIntentWrapper>,
    ) {
        let mut changesets = self.committed_changesets.lock().unwrap();
        base_intents.into_iter().for_each(|intent| {
            changesets.insert(intent.inner.id, intent);
        });
    }

    fn subscribe_for_results(
        &self,
    ) -> Receiver<
        tokio::sync::broadcast::Receiver<BroadcastedIntentExecutionResult>,
    > {
        let (_, receiver) = oneshot::channel();
        receiver
    }

    fn get_commit_statuses(
        &self,
        message_id: u64,
    ) -> Receiver<CommittorServiceResult<Vec<CommitStatusRow>>> {
        let (tx, rx) = oneshot::channel();

        let commit = self
            .committed_changesets
            .lock()
            .unwrap()
            .remove(&message_id);
        let Some(base_intent) = commit else {
            tx.send(Ok(vec![])).unwrap_or_else(|_| {
                log::error!("Failed to send commit status response");
            });
            return rx;
        };

        let status_rows =
            IntentPersisterImpl::create_commit_rows(&base_intent.inner);
        tx.send(Ok(status_rows)).unwrap_or_else(|_| {
            log::error!("Failed to send commit status response");
        });

        rx
    }

    fn get_commit_signatures(
        &self,
        _commit_id: u64,
        _pubkey: Pubkey,
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
impl BaseIntentCommittorExt for ChangesetCommittorStub {
    async fn schedule_base_intents_waiting(
        &self,
        l1_messages: Vec<ScheduledBaseIntentWrapper>,
    ) -> BaseIntentCommitorExtResult<Vec<BroadcastedIntentExecutionResult>>
    {
        self.commit_base_intent(l1_messages.clone());
        let res = l1_messages
            .into_iter()
            .map(|message| {
                Ok(ExecutionOutputWrapper {
                    id: message.inner.id,
                    output: ExecutionOutput {
                        commit_signature: Signature::new_unique(),
                        finalize_signature: Signature::new_unique(),
                    },
                    action_sent_transaction: Transaction::default(),
                    trigger_type: TriggerType::OnChain,
                    sent_commit: SentCommit {
                        message_id: message.inner.id,
                        slot: message.inner.slot,
                        blockhash: message.inner.blockhash,
                        payer: message.inner.payer,
                        requested_undelegation: message.inner.is_undelegate(),
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
