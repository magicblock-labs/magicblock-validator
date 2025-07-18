use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use log::{info, trace, warn};
use magicblock_program::magic_scheduled_l1_message::ScheduledL1Message;
use magicblock_rpc_client::MagicblockRpcClient;
use magicblock_table_mania::TableMania;
use solana_pubkey::Pubkey;
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
    utils::ScheduledMessageExt,
    ComputeBudgetConfig,
};

const SEMAPHORE_CLOSED_MSG: &str = "Executors semaphore closed!";

pub type BroadcasteddMessageExecutionResult = MessageExecutorResult<
    ExecutionOutput,
    Arc<crate::l1_message_executor::Error>,
>;

// TODO(edwin): reduce num of params: 1,2,3, could be united
pub(crate) struct CommitSchedulerWorker<D: DB, P: L1MessagesPersisterIface> {
    db: Arc<D>,
    l1_messages_persister: P,
    rpc_client: MagicblockRpcClient, // 1.
    table_mania: TableMania,         // 2.
    compute_budget_config: ComputeBudgetConfig, // 3.
    commit_id_tracker: CommitIdTracker,
    receiver: mpsc::Receiver<ScheduledL1Message>,

    // TODO(edwin): replace notify. issue: 2 simultaneous notifications
    notify: Arc<Notify>,
    executors_semaphore: Arc<Semaphore>,
    inner: Arc<Mutex<CommitSchedulerInner>>,
}

impl<D: DB, P: L1MessagesPersisterIface> CommitSchedulerWorker<D, P> {
    pub fn new(
        db: Arc<D>,
        l1_messages_persister: P,
        rpc_client: MagicblockRpcClient,
        table_mania: TableMania,
        compute_budget_config: ComputeBudgetConfig,
        receiver: mpsc::Receiver<ScheduledL1Message>,
    ) -> Self {
        // Number of executors that can send messages in parallel to L1
        const NUM_OF_EXECUTORS: u8 = 50;

        Self {
            db,
            l1_messages_persister,
            rpc_client: rpc_client.clone(),
            table_mania,
            compute_budget_config,
            commit_id_tracker: CommitIdTracker::new(rpc_client),
            receiver,
            notify: Arc::new(Notify::new()),
            executors_semaphore: Arc::new(Semaphore::new(
                NUM_OF_EXECUTORS as usize,
            )),
            inner: Arc::new(Mutex::new(CommitSchedulerInner::new())),
        }
    }

    /// Spawns `main_loop` and return `Receiver` listening to results
    pub fn spawn(
        mut self,
    ) -> broadcast::Receiver<BroadcasteddMessageExecutionResult> {
        let (result_sender, result_receiver) = broadcast::channel(100);
        tokio::spawn(self.main_loop(result_sender));

        result_receiver
    }

    /// Main loop that:
    /// 1. Handles & schedules incoming message
    /// 2. Finds available executor
    /// 3. Spawns execution of scheduled message
    async fn main_loop(
        mut self,
        result_sender: broadcast::Sender<BroadcasteddMessageExecutionResult>,
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
            let commit_ids =
                if let Some(pubkeys) = l1_message.get_committed_pubkeys() {
                    self.commit_id_tracker
                        .next_commit_ids(&pubkeys)
                        .await
                        .unwrap()
                } else {
                    // Pure L1Action, no commit ids used
                    HashMap::new()
                };
            let executor =
                L1MessageExecutor::<TransactionPreparatorV1, P>::new_v1(
                    self.rpc_client.clone(),
                    self.table_mania.clone(),
                    self.compute_budget_config.clone(),
                    self.l1_messages_persister.clone(),
                );

            // Spawn executor
            let inner = self.inner.clone();
            let notify = self.notify.clone();
            tokio::spawn(Self::execute(
                executor,
                l1_message,
                commit_ids,
                inner,
                permit,
                result_sender.clone(),
                notify,
            ));
        }
    }

    /// Returns [`ScheduledL1Message`] or None if all messages are blocked
    async fn next_scheduled_message(
        &mut self,
    ) -> Result<Option<ScheduledL1Message>, Error> {
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
    async fn get_new_message(&mut self) -> Result<ScheduledL1Message, Error> {
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
        executor: L1MessageExecutor<T, P>,
        l1_message: ScheduledL1Message,
        commit_ids: HashMap<Pubkey, u64>,
        inner_scheduler: Arc<Mutex<CommitSchedulerInner>>,
        execution_permit: OwnedSemaphorePermit,
        result_sender: broadcast::Sender<BroadcasteddMessageExecutionResult>,
        notify: Arc<Notify>,
    ) {
        let result = executor
            .execute(l1_message.clone(), commit_ids)
            .await
            .map_err(|err| Arc::new(err));
        // TODO: unwrap
        result_sender.send(result).unwrap();
        // Remove executed task from Scheduler to unblock other messages
        inner_scheduler
            .lock()
            .expect(POISONED_INNER_MSG)
            .complete(&l1_message);
        // Notify main loop that executor is done
        // This will trigger scheduling next message
        notify.notify_waiters();
        // Free worker
        drop(execution_permit);
    }

    async fn deduce_commit_ids(
        &mut self,
        l1_message: &ScheduledL1Message,
    ) -> HashMap<Pubkey, u64> {
        todo!()
    }
}

// Worker schedule:
// We have a pool of workers
// We are ready to accept message
// When we have a worker available to process it

/// 1. L1Messages arrive
/// 2. We call to schedule their execution
/// 3. Once landed we need to execute a sent tx on L2s

/// There's a part that schedules and sends TXs
/// L1MessageExecutor - runs Preparator + executes txs
/// Scheduler/MessageExecutionManager - Schedules execution of L1MessageExecutor
/// Committor - gets results and persists them
/// RemoteScheduledCommitsProcessor - just gets results and writes them to db
///
fn useless() {}

// Committor needs to get result of execution & persist it
// Committor needs to send results to Remote

// Could committor retry or handle failed execution somehow?
// Should that be a business of persister?
//

// Committor is used to manager TableMania
// On commits we fetch the state

// Does Remote care about readiness of particular task? No
// It just runs TXs where he commits results to l2.
// TODO(edwin): Remote takes single channel for result

// Does Committor care about readiness of particular task?
// It kinda doesn't
// Is it correct for MessageExecutionManager to be just a Stream?
//

// TODO(edwin): TransactionExecutor doesn't care about channels and shit
// It gets message, sends, retries & gives back result
fn useless2() {}
