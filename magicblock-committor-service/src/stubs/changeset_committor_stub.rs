use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Instant, SystemTime, UNIX_EPOCH},
};

use async_trait::async_trait;
use solana_pubkey::Pubkey;
use solana_sdk::signature::Signature;
use solana_transaction_status_client_types::{
    EncodedConfirmedTransactionWithStatusMeta, EncodedTransaction,
    EncodedTransactionWithStatusMeta,
};
use tokio::sync::{broadcast, oneshot};
use tokio_util::sync::{CancellationToken, WaitForCancellationFutureOwned};

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
    cancellation_token: CancellationToken,
    reserved_pubkeys_for_committee: Arc<Mutex<HashMap<Pubkey, Pubkey>>>,
    #[allow(clippy::type_complexity)]
    committed_changesets: Arc<Mutex<HashMap<u64, ScheduledBaseIntentWrapper>>>,
}

impl ChangesetCommittorStub {
    #[allow(clippy::len_without_is_empty)]
    pub fn len(&self) -> usize {
        self.committed_changesets.lock().unwrap().len()
    }
}

impl BaseIntentCommittor for ChangesetCommittorStub {
    fn reserve_pubkeys_for_committee(
        &self,
        committee: Pubkey,
        owner: Pubkey,
    ) -> oneshot::Receiver<CommittorServiceResult<Instant>> {
        let initiated = Instant::now();
        let (tx, rx) = oneshot::channel::<CommittorServiceResult<Instant>>();
        self.reserved_pubkeys_for_committee
            .lock()
            .unwrap()
            .insert(committee, owner);

        tx.send(Ok(initiated)).unwrap_or_else(|_| {
            log::error!("Failed to send response");
        });
        rx
    }

    fn schedule_base_intent(
        &self,
        base_intents: Vec<ScheduledBaseIntentWrapper>,
    ) -> oneshot::Receiver<CommittorServiceResult<()>> {
        let (sender, receiver) = oneshot::channel();
        let _ = sender.send(Ok(()));

        let mut changesets = self.committed_changesets.lock().unwrap();
        base_intents.into_iter().for_each(|intent| {
            changesets.insert(intent.inner.id, intent);
        });

        receiver
    }

    fn subscribe_for_results(
        &self,
    ) -> oneshot::Receiver<broadcast::Receiver<BroadcastedIntentExecutionResult>>
    {
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
            commit_stage_signature: Signature::new_unique(),
            finalize_stage_signature: Some(Signature::new_unique()),
            created_at: now(),
        };

        tx.send(Ok(Some(message_signature))).unwrap_or_else(|_| {
            log::error!("Failed to send bundle signatures response");
        });

        rx
    }

    fn get_transaction(
        &self,
        _: &Signature,
    ) -> oneshot::Receiver<
        CommittorServiceResult<EncodedConfirmedTransactionWithStatusMeta>,
    > {
        let (tx, rx) = oneshot::channel();
        if let Err(_err) =
            tx.send(Ok(EncodedConfirmedTransactionWithStatusMeta {
                slot: 0,
                transaction: EncodedTransactionWithStatusMeta {
                    transaction: EncodedTransaction::LegacyBinary(
                        "".to_string(),
                    ),
                    meta: None,
                    version: None,
                },
                block_time: None,
            }))
        {
            log::error!("Failed to send get transaction response");
        };

        rx
    }

    fn stop(&self) {
        self.cancellation_token.cancel();
    }

    fn stopped(&self) -> WaitForCancellationFutureOwned {
        self.cancellation_token.clone().cancelled_owned()
    }
}

#[async_trait]
impl BaseIntentCommittorExt for ChangesetCommittorStub {
    async fn schedule_base_intents_waiting(
        &self,
        base_intents: Vec<ScheduledBaseIntentWrapper>,
    ) -> BaseIntentCommitorExtResult<Vec<BroadcastedIntentExecutionResult>>
    {
        self.schedule_base_intent(base_intents.clone()).await??;
        let res = base_intents
            .into_iter()
            .map(|message| {
                Ok(ExecutionOutputWrapper {
                    id: message.inner.id,
                    output: ExecutionOutput::TwoStage {
                        commit_signature: Signature::new_unique(),
                        finalize_signature: Signature::new_unique(),
                    },
                    trigger_type: TriggerType::OnChain,
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
