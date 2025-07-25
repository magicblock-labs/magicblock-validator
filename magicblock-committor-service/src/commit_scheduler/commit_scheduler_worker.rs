use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex},
};

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

use crate::{
    commit_scheduler::{
        commit_id_tracker::CommitIdTracker,
        commit_scheduler_inner::{CommitSchedulerInner, POISONED_INNER_MSG},
        db::DB,
        Error,
    },
    l1_message_executor::{
        ExecutionOutput, L1MessageExecutor, MessageExecutorResult,
    },
    persist::L1MessagesPersisterIface,
    transaction_preperator::transaction_preparator::{
        TransactionPreparator, TransactionPreparatorV1,
    },
    types::{ScheduledL1MessageWrapper, TriggerType},
    utils::ScheduledMessageExt,
    ComputeBudgetConfig,
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

pub type BroadcastedError = (u64, Arc<crate::l1_message_executor::Error>);

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

pub(crate) struct CommitSchedulerWorker<D, P> {
    db: Arc<D>,
    l1_messages_persister: Option<P>,
    executor_factory: L1MessageExecutorFactory,
    commit_id_tracker: CommitIdTracker,
    receiver: mpsc::Receiver<ScheduledL1MessageWrapper>,

    // TODO(edwin): replace notify. issue: 2 simultaneous notifications
    notify: Arc<Notify>,
    executors_semaphore: Arc<Semaphore>,
    inner: Arc<Mutex<CommitSchedulerInner>>,
}

impl<D, P> CommitSchedulerWorker<D, P>
where
    D: DB,
    P: L1MessagesPersisterIface,
{
    pub fn new(
        db: Arc<D>,
        l1_messages_persister: Option<P>,
        rpc_client: MagicblockRpcClient,
        table_mania: TableMania,
        compute_budget_config: ComputeBudgetConfig,
        receiver: mpsc::Receiver<ScheduledL1MessageWrapper>,
    ) -> Self {
        // Number of executors that can send messages in parallel to L1
        const NUM_OF_EXECUTORS: u8 = 50;

        let executor_factory = L1MessageExecutorFactory {
            rpc_client: rpc_client.clone(),
            table_mania,
            compute_budget_config,
        };
        let commit_id_tracker = CommitIdTracker::new(rpc_client);
        Self {
            db,
            l1_messages_persister,
            executor_factory,
            commit_id_tracker,
            receiver,
            notify: Arc::new(Notify::new()),
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
            let executor = self.executor_factory.create_executor();
            let persister = self.l1_messages_persister.clone();
            let inner = self.inner.clone();
            let notify = self.notify.clone();

            tokio::spawn(Self::execute(
                executor,
                persister,
                l1_message,
                commit_ids,
                inner,
                permit,
                result_sender.clone(),
                notify,
            ));
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
                .blocked_messages_len();
            if num_blocked_messages < SCHEDULER_CAPACITY {
                true
            } else {
                warn!("Scheduler capacity exceeded: {}", num_blocked_messages);
                false
            }
        };
        let notify = self.notify.clone();
        let message = tokio::select! {
            // Notify polled first to prioritize unblocked messages over new one
            biased;
            _ = notify.notified() => {
                trace!("Worker executed L1Message, fetching new available one");
                self.inner.lock().expect(POISONED_INNER_MSG).pop_next_scheduled_message()
            },
            result = self.get_new_message(), if can_receive() => {
                let l1_message = result?;
                self.inner.lock().expect(POISONED_INNER_MSG).schedule(l1_message)
            }
        };

        Ok(message)
    }

    /// Returns [`ScheduledL1Message`] from external channel
    async fn get_new_message(
        &mut self,
    ) -> Result<ScheduledL1MessageWrapper, Error> {
        match self.receiver.try_recv() {
            Ok(val) => Ok(val),
            Err(TryRecvError::Empty) => {
                // Worker either cleaned-up congested channel and now need to clean-up DB
                // or we're just waiting on empty channel
                if let Some(l1_message) = self.db.pop_l1_message().await? {
                    Ok(l1_message)
                } else {
                    self.receiver.recv().await.ok_or(Error::ChannelClosed)
                }
            }
            Err(TryRecvError::Disconnected) => Err(Error::ChannelClosed),
        }
    }

    /// Wrapper on [`L1MessageExecutor`] that handles its results and drops execution permit
    async fn execute<T: TransactionPreparator>(
        executor: L1MessageExecutor<T>,
        persister: Option<P>,
        l1_message: ScheduledL1MessageWrapper,
        commit_ids: HashMap<Pubkey, u64>,
        inner_scheduler: Arc<Mutex<CommitSchedulerInner>>,
        execution_permit: OwnedSemaphorePermit,
        result_sender: broadcast::Sender<BroadcastedMessageExecutionResult>,
        notify: Arc<Notify>,
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
        // Notify main loop that executor is done
        // This will trigger scheduling next message
        notify.notify_waiters();
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

/// Dummy struct to implify signatur
struct L1MessageExecutorFactory {
    rpc_client: MagicblockRpcClient,
    table_mania: TableMania,
    compute_budget_config: ComputeBudgetConfig,
}

impl L1MessageExecutorFactory {
    pub fn create_executor(
        &self,
    ) -> L1MessageExecutor<TransactionPreparatorV1> {
        L1MessageExecutor::<TransactionPreparatorV1>::new_v1(
            self.rpc_client.clone(),
            self.table_mania.clone(),
            self.compute_budget_config.clone(),
        )
    }
}
