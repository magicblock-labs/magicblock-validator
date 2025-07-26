use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex},
};
use futures_util::stream::FuturesUnordered;
use futures_util::StreamExt;
use log::{error, info, trace, warn};
use magicblock_program::SentCommit;
use magicblock_rpc_client::MagicblockRpcClient;
use magicblock_table_mania::TableMania;
use solana_pubkey::Pubkey;
use solana_sdk::transaction::Transaction;
use tokio::sync::{
    broadcast, mpsc, mpsc::error::TryRecvError, Notify, OwnedSemaphorePermit,
    Semaphore,
};
use tokio::task::JoinHandle;
use crate::{
    commit_scheduler::{
        commit_id_tracker::CommitIdTracker,
        commit_scheduler_inner::{CommitSchedulerInner, POISONED_INNER_MSG},
        db::DB,
        Error,
    },
    message_executor::{
        error::MessageExecutorResult,
        message_executor_factory::MessageExecutorFactory, ExecutionOutput,
        MessageExecutor,
    },
    persist::L1MessagesPersisterIface,
    types::{ScheduledL1MessageWrapper, TriggerType},
    utils::ScheduledMessageExt,
};

const SEMAPHORE_CLOSED_MSG: &str = "Executors semaphore closed!";

// TODO(edwin): rename
#[derive(Clone)]
pub struct ExecutionOutputWrapper {
    pub id: u64,
    pub output: ExecutionOutput,
    pub action_sent_transaction: Transaction,
    pub sent_commit: SentCommit,
    pub trigger_type: TriggerType,
}

pub type BroadcastedError = (u64, Arc<crate::message_executor::error::Error>);

pub type BroadcastedMessageExecutionResult =
    MessageExecutorResult<ExecutionOutputWrapper, BroadcastedError>;

/// Struct that exposes only `subscribe` method of `broadcast::Sender` for better isolation
pub struct ResultSubscriber(
    broadcast::Sender<BroadcastedMessageExecutionResult>,
);
impl ResultSubscriber {
    pub fn subscribe(
        &self,
    ) -> broadcast::Receiver<BroadcastedMessageExecutionResult> {
        self.0.subscribe()
    }
}

pub(crate) struct CommitSchedulerWorker<D, P, F, C> {
    db: Arc<D>,
    l1_messages_persister: Option<P>,
    executor_factory: F,
    commit_id_tracker: C,
    receiver: mpsc::Receiver<ScheduledL1MessageWrapper>,

    // TODO(edwin): replace notify. issue: 2 simultaneous notifications
    running_executors: FuturesUnordered<JoinHandle<()>>,
    executors_semaphore: Arc<Semaphore>,
    inner: Arc<Mutex<CommitSchedulerInner>>,
}

impl<D, P, F, E, C> CommitSchedulerWorker<D, P, F, C>
where
    D: DB,
    P: L1MessagesPersisterIface,
    F: MessageExecutorFactory<Executor = E> + Send + Sync + 'static,
    E: MessageExecutor,
    C: CommitIdTracker + Send + Sync + 'static,
{
    pub fn new(
        db: Arc<D>,
        executor_factory: F,
        commit_id_tracker: C,
        l1_messages_persister: Option<P>,
        receiver: mpsc::Receiver<ScheduledL1MessageWrapper>,
    ) -> Self {
        // Number of executors that can send messages in parallel to L1
        const NUM_OF_EXECUTORS: u8 = 50;

        Self {
            db,
            l1_messages_persister,
            executor_factory,
            commit_id_tracker,
            receiver,
            running_executors: FuturesUnordered::new(),
            executors_semaphore: Arc::new(Semaphore::new(
                NUM_OF_EXECUTORS as usize,
            )),
            inner: Arc::new(Mutex::new(CommitSchedulerInner::new())),
        }
    }

    /// Spawns `main_loop` and return `Receiver` listening to results
    pub fn spawn(self) -> ResultSubscriber {
        let (result_sender, _) = broadcast::channel(100);
        tokio::spawn(self.main_loop(result_sender.clone()));

        ResultSubscriber(result_sender)
    }

    /// Main loop that:
    /// 1. Handles & schedules incoming message
    /// 2. Finds available executor
    /// 3. Spawns execution of scheduled message
    async fn main_loop(
        mut self,
        result_sender: broadcast::Sender<BroadcastedMessageExecutionResult>,
    ) {
        loop {
            // TODO: unwraps
            let l1_message = self.next_scheduled_message().await.unwrap();
            let Some(l1_message) = l1_message else {
                // Messages are blocked, skipping
                info!("Could not schedule any messages, as all of them are blocked!");
                continue;
            };

            // Waiting until there's available executor
            let permit = self
                .executors_semaphore
                .clone()
                .acquire_owned()
                .await
                .expect(SEMAPHORE_CLOSED_MSG);

            // Prepare data for execution
            let commit_ids = if let Some(pubkeys) =
                l1_message.scheduled_l1_message.get_committed_pubkeys()
            {
                let commit_ids = self
                    .commit_id_tracker
                    .next_commit_ids(&pubkeys)
                    .await
                    .unwrap();
                // Persist data
                commit_ids
                    .iter()
                    .for_each(|(pubkey, commit_id) | {
                        let Some(persistor) = &self.l1_messages_persister else {
                            return;
                        };
                        if let Err(err) = persistor.set_commit_id(l1_message.scheduled_l1_message.id, pubkey, *commit_id) {
                            error!("Failed to persist commit id: {}, for message id: {} with pubkey {}: {}", commit_id, l1_message.scheduled_l1_message.id, pubkey, err);
                        }
                    });

                commit_ids
            } else {
                // Pure L1Action, no commit ids used
                HashMap::new()
            };

            // Spawn executor
            let executor = self.executor_factory.create_instance();
            let persister = self.l1_messages_persister.clone();
            let inner = self.inner.clone();

            let handle = tokio::spawn(Self::execute(
                executor,
                persister,
                l1_message,
                commit_ids,
                inner,
                permit,
                result_sender.clone(),
            ));

            self.running_executors.push(handle);
        }
    }

    /// Returns [`ScheduledL1MessageWrapper`] or None if all messages are blocked
    async fn next_scheduled_message(
        &mut self,
    ) -> Result<Option<ScheduledL1MessageWrapper>, Error> {
        // Limit on number of messages that can be stored in scheduler
        const SCHEDULER_CAPACITY: usize = 1000;

        let can_receive = || {
            let num_blocked_messages = self
                .inner
                .lock()
                .expect(POISONED_INNER_MSG)
                .messages_blocked();
            if num_blocked_messages < SCHEDULER_CAPACITY {
                true
            } else {
                warn!("Scheduler capacity exceeded: {}", num_blocked_messages);
                false
            }
        };

        let running_executors = &mut self.running_executors;
        let receiver = &mut self.receiver;
        let db = &self.db;
        let message = tokio::select! {
            // Notify polled first to prioritize unblocked messages over new one
            biased;
            Some(result) = running_executors.next() => {
                if let Err(err) = result {
                    error!("Executor failed to complete: {}", err);
                };
                trace!("Worker executed L1Message, fetching new available one");
                self.inner.lock().expect(POISONED_INNER_MSG).pop_next_scheduled_message()
            },
            result = Self::get_new_message(receiver, db), if can_receive() => {
                let l1_message = result?;
                self.inner.lock().expect(POISONED_INNER_MSG).schedule(l1_message)
            },
            else => {
                // Shouldn't be possible
                // If no executors spawned -> we can receive
                // If can't receive -> there are running executors
                unreachable!("next_scheduled_message")
            }
        };

        Ok(message)
    }

    /// Returns [`ScheduledL1Message`] from external channel
    async fn get_new_message(
        receiver: &mut mpsc::Receiver<ScheduledL1MessageWrapper>,
        db: &Arc<D>
    ) -> Result<ScheduledL1MessageWrapper, Error> {
        match receiver.try_recv() {
            Ok(val) => Ok(val),
            Err(TryRecvError::Empty) => {
                // Worker either cleaned-up congested channel and now need to clean-up DB
                // or we're just waiting on empty channel
                if let Some(l1_message) = db.pop_l1_message().await? {
                    Ok(l1_message)
                } else {
                    receiver.recv().await.ok_or(Error::ChannelClosed)
                }
            }
            Err(TryRecvError::Disconnected) => Err(Error::ChannelClosed),
        }
    }

    /// Wrapper on [`L1MessageExecutor`] that handles its results and drops execution permit
    async fn execute(
        executor: E,
        persister: Option<P>,
        l1_message: ScheduledL1MessageWrapper,
        commit_ids: HashMap<Pubkey, u64>,
        inner_scheduler: Arc<Mutex<CommitSchedulerInner>>,
        execution_permit: OwnedSemaphorePermit,
        result_sender: broadcast::Sender<BroadcastedMessageExecutionResult>,
    ) {
        let result = executor
            .execute(
                l1_message.scheduled_l1_message.clone(),
                commit_ids,
                persister,
            )
            .await
            .inspect_err(|err| error!("Failed to execute L1Message: {:?}", err))
            .map(|raw_result| {
                Self::map_execution_outcome(&l1_message, raw_result)
            })
            .map_err(|err| (l1_message.scheduled_l1_message.id, Arc::new(err)));

        // Broadcast result to subscribers
        if let Err(err) = result_sender.send(result) {
            error!("Failed to broadcast result: {}", err);
        }
        // Remove executed task from Scheduler to unblock other messages
        inner_scheduler
            .lock()
            .expect(POISONED_INNER_MSG)
            .complete(&l1_message.scheduled_l1_message);

        // Free worker
        drop(execution_permit);
    }

    /// Maps output of `L1MessageExecutor` to final result
    fn map_execution_outcome(
        l1_message: &ScheduledL1MessageWrapper,
        raw_outcome: ExecutionOutput,
    ) -> ExecutionOutputWrapper {
        let ScheduledL1MessageWrapper {
            scheduled_l1_message,
            feepayers,
            excluded_pubkeys,
            trigger_type,
        } = l1_message;
        let included_pubkeys = if let Some(included_pubkeys) =
            scheduled_l1_message.get_committed_pubkeys()
        {
            included_pubkeys
        } else {
            // Case with standalone actions
            vec![]
        };
        let requested_undelegation = scheduled_l1_message.is_undelegate();
        let chain_signatures =
            vec![raw_outcome.commit_signature, raw_outcome.finalize_signature];

        let sent_commit = SentCommit {
            message_id: scheduled_l1_message.id,
            slot: scheduled_l1_message.slot,
            blockhash: scheduled_l1_message.blockhash,
            payer: scheduled_l1_message.payer,
            included_pubkeys,
            excluded_pubkeys: excluded_pubkeys.clone(),
            feepayers: HashSet::from_iter(feepayers.iter().cloned()),
            requested_undelegation,
            chain_signatures,
        };

        ExecutionOutputWrapper {
            id: scheduled_l1_message.id,
            output: raw_outcome,
            action_sent_transaction: scheduled_l1_message
                .action_sent_transaction
                .clone(),
            trigger_type: *trigger_type,
            sent_commit,
        }
    }
}

/// Worker tests
#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use async_trait::async_trait;
    use magicblock_program::magic_scheduled_l1_message::ScheduledL1Message;
    use solana_pubkey::pubkey;
    use solana_sdk::{signature::Signature, signer::SignerError};
    use tokio::sync::mpsc;

    use super::*;
    use crate::{
        commit_scheduler::{
            commit_id_tracker::{CommitIdTracker, CommitIdTrackerResult},
            commit_scheduler_inner::create_test_message,
            db::{DummyDB, DB},
        },
        message_executor::error::{
            Error as ExecutorError, InternalError, MessageExecutorResult,
        },
        persist::L1MessagePersister,
    };

    type MockCommitSchedulerWorker = CommitSchedulerWorker<
        DummyDB,
        L1MessagePersister,
        MockMessageExecutorFactory,
        MockCommitIdTracker,
    >;
    fn setup_worker(should_fail: bool) -> (
        mpsc::Sender<ScheduledL1MessageWrapper>,
        MockCommitSchedulerWorker,
    ) {
        let (sender, receiver) = mpsc::channel(10);

        let db = Arc::new(DummyDB::new());
        let executor_factory = if !should_fail {
            MockMessageExecutorFactory::new()
        } else {
            MockMessageExecutorFactory::new_failing()
        };
        let commit_id_tracker = MockCommitIdTracker::new();
        let worker = CommitSchedulerWorker::new(
            db.clone(),
            executor_factory,
            commit_id_tracker,
            None::<L1MessagePersister>,
            receiver,
        );

        (sender, worker)
    }

    #[tokio::test]
    async fn test_worker_processes_messages() {
        let (sender, worker) = setup_worker(false);
        let result_subscriber = worker.spawn();
        let mut result_receiver = result_subscriber.subscribe();

        // Send a test message
        let msg = create_test_message(
            1,
            &[pubkey!("1111111111111111111111111111111111111111111")],
        );
        sender.send(msg.clone()).await.unwrap();

        // Verify the message was processed
        let result = result_receiver.recv().await.unwrap();
        assert!(result.is_ok());
        let output = result.unwrap();
        assert_eq!(output.id, 1);
    }

    #[tokio::test]
    async fn test_worker_handles_conflicting_messages() {
        let (sender, worker) = setup_worker(false);
        let result_subscriber = worker.spawn();
        let mut result_receiver = result_subscriber.subscribe();

        // Send two conflicting messages
        let pubkey = pubkey!("1111111111111111111111111111111111111111111");
        let msg1 = create_test_message(1, &[pubkey]);
        let msg2 = create_test_message(2, &[pubkey]);

        sender.send(msg1.clone()).await.unwrap();
        sender.send(msg2.clone()).await.unwrap();

        // First message should be processed immediately
        let result1 = result_receiver.recv().await.unwrap();
        assert!(result1.is_ok());
        assert_eq!(result1.unwrap().id, 1);

        // Second message should be processed after first completes
        let result2 = result_receiver.recv().await.unwrap();
        assert!(result2.is_ok());
        assert_eq!(result2.unwrap().id, 2);
    }

    #[tokio::test]
    async fn test_worker_handles_executor_failure() {
        let (sender, worker) = setup_worker(true);
        let result_subscriber = worker.spawn();
        let mut result_receiver = result_subscriber.subscribe();

        // Send a test message that will fail
        let msg = create_test_message(
            1,
            &[pubkey!("1111111111111111111111111111111111111111111")],
        );
        sender.send(msg.clone()).await.unwrap();

        // Verify the failure was properly reported
        let result = result_receiver.recv().await.unwrap();
        let Err((id, err)) = result else {
            panic!();
        };
        assert_eq!(id, 1);
        assert_eq!(
            err.to_string(),
            "FailedToCommitError: SignerError: custom error: oops"
        );
    }

    #[tokio::test]
    async fn test_worker_falls_back_to_db_when_channel_empty() {
        let (sender, worker) = setup_worker(false);

        // Add a message to the DB
        let msg = create_test_message(
            1,
            &[pubkey!("1111111111111111111111111111111111111111111")],
        );
        worker.db.store_l1_message(msg.clone()).await.unwrap();

        // Start worker
        let result_subscriber = worker.spawn();
        let mut result_receiver = result_subscriber.subscribe();

        // Verify the message from DB was processed
        let result = result_receiver.recv().await.unwrap();
        assert!(result.is_ok());
        assert_eq!(result.unwrap().id, 1);
    }

    // Mock implementations for testing
    pub struct MockMessageExecutorFactory {
        should_fail: bool,
    }

    impl MockMessageExecutorFactory {
        pub fn new() -> Self {
            Self { should_fail: false }
        }

        pub fn new_failing() -> Self {
            Self { should_fail: true }
        }
    }

    impl MessageExecutorFactory for MockMessageExecutorFactory {
        type Executor = MockMessageExecutor;

        fn create_instance(&self) -> Self::Executor {
            MockMessageExecutor {
                should_fail: self.should_fail,
            }
        }
    }

    pub struct MockMessageExecutor {
        should_fail: bool,
    }

    #[async_trait]
    impl MessageExecutor for MockMessageExecutor {
        async fn execute<P: L1MessagesPersisterIface>(
            &self,
            l1_message: ScheduledL1Message,
            _commit_ids: HashMap<Pubkey, u64>,
            _persister: Option<P>,
        ) -> MessageExecutorResult<ExecutionOutput> {
            // TODO: add sleep
            if self.should_fail {
                Err(ExecutorError::FailedToCommitError {
                    err: InternalError::SignerError(SignerError::Custom(
                        "oops".to_string(),
                    )),
                    signature: None,
                })
            } else {
                Ok(ExecutionOutput {
                    commit_signature: Signature::default(),
                    finalize_signature: Signature::default(),
                })
            }
        }
    }

    pub struct MockCommitIdTracker;
    impl MockCommitIdTracker {
        pub fn new() -> Self {
            Self
        }
    }

    #[async_trait]
    impl CommitIdTracker for MockCommitIdTracker {
        async fn next_commit_ids(
            &mut self,
            pubkeys: &[Pubkey],
        ) -> CommitIdTrackerResult<HashMap<Pubkey, u64>> {
            Ok(pubkeys.iter().map(|&k| (k, 1)).collect())
        }

        fn peek_commit_id(&self, _pubkey: &Pubkey) -> Option<&u64> {
            None
        }
    }
}
