use std::{
    collections::{hash_map::Entry, HashMap},
    ops::Deref,
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use futures_util::future::join_all;
use log::error;
use solana_pubkey::Pubkey;
use tokio::sync::{broadcast, oneshot, oneshot::error::RecvError};

use crate::{
    commit_scheduler::BroadcastedMessageExecutionResult,
    error::CommittorServiceResult,
    persist::{CommitStatusRow, MessageSignatures},
    types::ScheduledL1MessageWrapper,
    L1MessageCommittor,
};

const POISONED_MUTEX_MSG: &str =
    "CommittorServiceExt pending messages mutex poisoned!";

#[async_trait]
pub trait L1MessageCommittorExt: L1MessageCommittor {
    /// Schedules l1 messages and waits for their results
    async fn commit_l1_messages_waiting(
        &self,
        l1_messages: Vec<ScheduledL1MessageWrapper>,
    ) -> L1MessageCommitorExtResult<Vec<BroadcastedMessageExecutionResult>>;
}

type MessageResultListener = oneshot::Sender<BroadcastedMessageExecutionResult>;
pub struct CommittorServiceExt<CC> {
    inner: Arc<CC>,
    pending_messages: Arc<Mutex<HashMap<u64, MessageResultListener>>>,
}

impl<CC: L1MessageCommittor> CommittorServiceExt<CC> {
    pub fn new(inner: Arc<CC>) -> Self {
        let pending_messages = Arc::new(Mutex::new(HashMap::new()));
        let results_subscription = inner.subscribe_for_results();
        tokio::spawn(Self::dispatcher(
            results_subscription,
            pending_messages.clone(),
        ));

        Self {
            inner,
            pending_messages,
        }
    }

    async fn dispatcher(
        results_subscription: oneshot::Receiver<
            broadcast::Receiver<BroadcastedMessageExecutionResult>,
        >,
        pending_message: Arc<Mutex<HashMap<u64, MessageResultListener>>>,
    ) {
        let mut results_subscription = results_subscription.await.unwrap();
        while let Ok(execution_result) = results_subscription.recv().await {
            let id = match &execution_result {
                Ok(value) => value.id,
                Err(err) => err.0,
            };

            let sender = if let Some(sender) = pending_message
                .lock()
                .expect(POISONED_MUTEX_MSG)
                .remove(&id)
            {
                sender
            } else {
                continue;
            };

            if let Err(_) = sender.send(execution_result) {
                error!("Failed to send L1Message execution result to listener");
            }
        }
    }
}

#[async_trait]
impl<CC: L1MessageCommittor> L1MessageCommittorExt for CommittorServiceExt<CC> {
    async fn commit_l1_messages_waiting(
        &self,
        l1_messages: Vec<ScheduledL1MessageWrapper>,
    ) -> L1MessageCommitorExtResult<Vec<BroadcastedMessageExecutionResult>>
    {
        let mut receivers = {
            let mut pending_messages =
                self.pending_messages.lock().expect(POISONED_MUTEX_MSG);

            l1_messages
                .iter()
                .map(|l1_message| {
                    let (sender, receiver) = oneshot::channel();
                    match pending_messages
                        .entry(l1_message.scheduled_l1_message.id)
                    {
                        Entry::Vacant(vacant) => {
                            vacant.insert(sender);
                            Ok(receiver)
                        }
                        Entry::Occupied(_) => {
                            Err(Error::RepeatingMessageError(
                                l1_message.scheduled_l1_message.id,
                            ))
                        }
                    }
                })
                .collect::<Result<Vec<_>, _>>()?
        };

        let results = join_all(receivers.into_iter())
            .await
            .into_iter()
            .collect::<Result<Vec<_>, RecvError>>()?;

        Ok(results)
    }
}

impl<CC: L1MessageCommittor> L1MessageCommittor for CommittorServiceExt<CC> {
    fn reserve_pubkeys_for_committee(
        &self,
        committee: Pubkey,
        owner: Pubkey,
    ) -> oneshot::Receiver<CommittorServiceResult<()>> {
        self.inner.reserve_pubkeys_for_committee(committee, owner)
    }

    fn commit_l1_messages(&self, l1_messages: Vec<ScheduledL1MessageWrapper>) {
        self.inner.commit_l1_messages(l1_messages)
    }

    fn subscribe_for_results(
        &self,
    ) -> oneshot::Receiver<broadcast::Receiver<BroadcastedMessageExecutionResult>>
    {
        self.inner.subscribe_for_results()
    }

    fn get_commit_statuses(
        &self,
        message_id: u64,
    ) -> oneshot::Receiver<CommittorServiceResult<Vec<CommitStatusRow>>> {
        self.inner.get_commit_statuses(message_id)
    }

    fn get_commit_signatures(
        &self,
        commit_id: u64,
        pubkey: Pubkey,
    ) -> oneshot::Receiver<CommittorServiceResult<Option<MessageSignatures>>>
    {
        self.inner.get_commit_signatures(commit_id, pubkey)
    }
}

impl<CC: L1MessageCommittor> Deref for CommittorServiceExt<CC> {
    type Target = Arc<CC>;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("Attempt to schedule already scheduled message id: {0}")]
    RepeatingMessageError(u64),
    #[error("RecvError: {0}")]
    RecvError(#[from] RecvError),
}

pub type L1MessageCommitorExtResult<T, E = Error> = Result<T, E>;
