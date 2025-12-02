use std::{
    collections::{hash_map::Entry, HashMap},
    ops::Deref,
    sync::{Arc, Mutex},
    time::Instant,
};

use async_trait::async_trait;
use futures_util::future::join_all;
use log::{error, info};
use solana_pubkey::Pubkey;
use solana_sdk::signature::Signature;
use solana_transaction_status_client_types::EncodedConfirmedTransactionWithStatusMeta;
use tokio::sync::{broadcast, oneshot, oneshot::error::RecvError};
use tokio_util::sync::WaitForCancellationFutureOwned;

use crate::{
    error::{CommittorServiceError, CommittorServiceResult},
    intent_execution_manager::BroadcastedIntentExecutionResult,
    persist::{CommitStatusRow, MessageSignatures},
    types::ScheduledBaseIntentWrapper,
    BaseIntentCommittor,
};

const POISONED_MUTEX_MSG: &str =
    "CommittorServiceExt pending messages mutex poisoned!";

#[async_trait]
pub trait BaseIntentCommittorExt: BaseIntentCommittor {
    /// Schedules Base Intents and waits for their results
    async fn schedule_base_intents_waiting(
        &self,
        base_intents: Vec<ScheduledBaseIntentWrapper>,
    ) -> BaseIntentCommitorExtResult<Vec<BroadcastedIntentExecutionResult>>;
}

type MessageResultListener = oneshot::Sender<BroadcastedIntentExecutionResult>;
pub struct CommittorServiceExt<CC> {
    inner: Arc<CC>,
    pending_messages: Arc<Mutex<HashMap<u64, MessageResultListener>>>,
}

impl<CC: BaseIntentCommittor> CommittorServiceExt<CC> {
    pub fn new(inner: Arc<CC>) -> Self {
        let pending_messages = Arc::new(Mutex::new(HashMap::new()));
        let results_subscription = inner.subscribe_for_results();
        let committor_stopped = inner.stopped();
        tokio::spawn(Self::dispatcher(
            committor_stopped,
            results_subscription,
            pending_messages.clone(),
        ));

        Self {
            inner,
            pending_messages,
        }
    }

    async fn dispatcher(
        committor_stopped: WaitForCancellationFutureOwned,
        results_subscription: oneshot::Receiver<
            broadcast::Receiver<BroadcastedIntentExecutionResult>,
        >,
        pending_message: Arc<Mutex<HashMap<u64, MessageResultListener>>>,
    ) {
        let mut results_subscription = results_subscription.await.unwrap();

        tokio::pin!(committor_stopped);
        loop {
            let execution_result = tokio::select! {
                biased;
                _ = &mut committor_stopped => {
                    info!("Committor service stopped, stopping Committor extension");
                    return;
                }
                execution_result = results_subscription.recv() => {
                    match execution_result {
                        Ok(result) => result,
                        Err(broadcast::error::RecvError::Closed) => {
                            info!("Intent execution got shutdown, shutting down result Committor extension!");
                            break;
                        }
                        Err(broadcast::error::RecvError::Lagged(skipped)) => {
                            // SAFETY: not really feasible to happen as this function is way faster than Intent execution
                            // requires investigation if ever happens!
                            error!("CommittorServiceExt lags behind Intent execution! skipped: {}", skipped);
                            continue;
                        }
                    }
                }
            };

            let sender = if let Some(sender) = pending_message
                .lock()
                .expect(POISONED_MUTEX_MSG)
                .remove(&execution_result.id)
            {
                sender
            } else {
                continue;
            };

            if sender.send(execution_result).is_err() {
                error!(
                    "Failed to send BaseIntent execution result to listener"
                );
            }
        }
    }
}

#[async_trait]
impl<CC: BaseIntentCommittor> BaseIntentCommittorExt
    for CommittorServiceExt<CC>
{
    async fn schedule_base_intents_waiting(
        &self,
        base_intents: Vec<ScheduledBaseIntentWrapper>,
    ) -> BaseIntentCommitorExtResult<Vec<BroadcastedIntentExecutionResult>>
    {
        // Critical section
        let receivers = {
            let mut pending_messages =
                self.pending_messages.lock().expect(POISONED_MUTEX_MSG);

            base_intents
                .iter()
                .map(|intent| {
                    let (sender, receiver) = oneshot::channel();
                    match pending_messages.entry(intent.inner.id) {
                        Entry::Vacant(vacant) => {
                            vacant.insert(sender);
                            Ok(receiver)
                        }
                        Entry::Occupied(_) => Err(
                            CommittorServiceExtError::RepeatingMessageError(
                                intent.inner.id,
                            ),
                        ),
                    }
                })
                .collect::<Result<Vec<_>, _>>()?
        };

        self.schedule_base_intent(base_intents).await??;
        let results = join_all(receivers.into_iter())
            .await
            .into_iter()
            .collect::<Result<Vec<_>, RecvError>>()?;

        Ok(results)
    }
}

impl<CC: BaseIntentCommittor> BaseIntentCommittor for CommittorServiceExt<CC> {
    fn reserve_pubkeys_for_committee(
        &self,
        committee: Pubkey,
        owner: Pubkey,
    ) -> oneshot::Receiver<CommittorServiceResult<Instant>> {
        self.inner.reserve_pubkeys_for_committee(committee, owner)
    }

    fn schedule_base_intent(
        &self,
        base_intents: Vec<ScheduledBaseIntentWrapper>,
    ) -> oneshot::Receiver<CommittorServiceResult<()>> {
        self.inner.schedule_base_intent(base_intents)
    }

    fn subscribe_for_results(
        &self,
    ) -> oneshot::Receiver<broadcast::Receiver<BroadcastedIntentExecutionResult>>
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

    fn get_transaction(
        &self,
        signature: &Signature,
    ) -> oneshot::Receiver<
        CommittorServiceResult<EncodedConfirmedTransactionWithStatusMeta>,
    > {
        self.inner.get_transaction(signature)
    }

    fn stop(&self) {
        self.inner.stop();
    }

    fn stopped(&self) -> WaitForCancellationFutureOwned {
        self.inner.stopped()
    }
}

impl<CC: BaseIntentCommittor> Deref for CommittorServiceExt<CC> {
    type Target = Arc<CC>;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

#[derive(thiserror::Error, Debug)]
pub enum CommittorServiceExtError {
    #[error("Attempt to schedule already scheduled message id: {0}")]
    RepeatingMessageError(u64),
    #[error("RecvError: {0}")]
    RecvError(#[from] RecvError),
    #[error("CommittorServiceError: {0:?}")]
    CommittorServiceError(#[from] CommittorServiceError),
}

pub type BaseIntentCommitorExtResult<T, E = CommittorServiceExtError> =
    Result<T, E>;
