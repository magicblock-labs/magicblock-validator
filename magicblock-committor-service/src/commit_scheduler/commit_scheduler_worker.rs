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
        commit_scheduler_inner::{CommitSchedulerInner, POISONED_INNER_MSG},
        db::DB,
        Error,
    },
    l1_message_executor::{
        ExecutionOutput, L1MessageExecutor, MessageExecutorResult,
    },
    transaction_preperator::transaction_preparator::TransactionPreparator,
    ComputeBudgetConfig,
};

const SEMAPHORE_CLOSED_MSG: &str = "Executors semaphore closed!";

pub(crate) struct CommitSchedulerWorker<D: DB> {
    db: Arc<D>,
    rpc_client: MagicblockRpcClient,
    table_mania: TableMania,
    compute_budget_config: ComputeBudgetConfig,
    receiver: mpsc::Receiver<ScheduledL1Message>,

    // TODO(edwin): replace notify. issue: 2 simultaneous notifications
    notify: Arc<Notify>,
    executors_semaphore: Arc<Semaphore>,
    inner: Arc<Mutex<CommitSchedulerInner>>,
}

impl<D: DB> CommitSchedulerWorker<D> {
    pub fn new(
        db: Arc<D>,
        rpc_client: MagicblockRpcClient,
        table_mania: TableMania,
        compute_budget_config: ComputeBudgetConfig,
        receiver: mpsc::Receiver<ScheduledL1Message>,
    ) -> Self {
        // Number of executors that can send messages in parallel to L1
        const NUM_OF_EXECUTORS: u8 = 50;

        Self {
            db,
            rpc_client,
            table_mania,
            compute_budget_config,
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
    ) -> broadcast::Receiver<MessageExecutorResult<ExecutionOutput>> {
        let (sender, receiver) = broadcast::channel(100);
        tokio::spawn(self.main_loop(sender));

        receiver
    }

    /// Main loop that:
    /// 1. Handles & schedules incoming message
    /// 2. Finds available executor
    /// 3. Spawns execution of scheduled message
    async fn main_loop(
        mut self,
        result_sender: broadcast::Sender<
            MessageExecutorResult<ExecutionOutput>,
        >,
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
            let commit_ids = self.deduce_commit_ids(&l1_message).await;
            let executor = L1MessageExecutor::new_v1(
                self.rpc_client.clone(),
                self.table_mania.clone(),
                self.compute_budget_config.clone(),
            );

            // Spawn executor
            tokio::spawn(Self::execute(
                executor,
                l1_message,
                commit_ids,
                self.inner.clone(),
                permit,
                result_sender.clone(),
                self.notify.clone(),
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
        let message = tokio::select! {
            // Notify polled first to prioritize unblocked messages over new one
            biased;
            _ = self.notify.notified() => {
                trace!("Worker executed L1Message, fetching new available one");
                // TODO(edwin): ensure that worker properly completes message in inner schedyler
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
        executor: L1MessageExecutor<T>,
        l1_message: ScheduledL1Message,
        commit_ids: HashMap<Pubkey, u64>,
        inner_scheduler: Arc<Mutex<CommitSchedulerInner>>,
        execution_permit: OwnedSemaphorePermit,
        result_sender: broadcast::Sender<
            MessageExecutorResult<ExecutionOutput>,
        >,
        notify: Arc<Notify>,
    ) {
        let result = executor.execute(l1_message.clone(), commit_ids).await;
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
