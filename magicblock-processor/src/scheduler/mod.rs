//! The central transaction scheduler and its event loop.
//!
//! This module is the entry point for all transactions into the processing pipeline.
//! It is responsible for creating and managing a pool of `TransactionExecutor`
//! workers and dispatching transactions to them for execution.

use std::sync::{atomic::AtomicUsize, Arc, RwLock};

use coordinator::{ExecutionCoordinator, TransactionWithId};
use locks::{ExecutorId, MAX_SVM_EXECUTORS};
use log::info;
use magicblock_core::link::transactions::{
    ProcessableTransaction, TransactionToProcessRx,
};
use magicblock_ledger::LatestBlock;
use solana_program_runtime::loaded_programs::ProgramCache;
use state::TransactionSchedulerState;
use tokio::{
    runtime::Builder,
    sync::mpsc::{channel, Receiver, Sender},
};

use crate::executor::{SimpleForkGraph, TransactionExecutor};

/// Each executor has a channel capacity of 1, as it
/// can only process one transaction at a time.
const EXECUTOR_QUEUE_CAPACITY: usize = 1;

/// The central transaction scheduler responsible for distributing work to a
/// pool of `TransactionExecutor` workers.
///
/// This struct acts as the single entry point for all transactions entering the processing
/// pipeline. It receives transactions from a global queue and dispatches them to available
/// worker threads for execution or simulation.
pub struct TransactionScheduler {
    /// Manages the state of all executors, including locks and blocked transactions.
    coordinator: ExecutionCoordinator,
    /// The receiving end of the global queue for all new transactions.
    transactions_rx: TransactionToProcessRx,
    /// A channel that receives readiness notifications from workers,
    /// indicating they are free to accept new work.
    ready_rx: Receiver<ExecutorId>,
    /// A list of sender channels, one for each `TransactionExecutor` worker.
    executors: Vec<Sender<ProcessableTransaction>>,
    /// A handle to the globally shared cache for loaded BPF programs.
    program_cache: Arc<RwLock<ProgramCache<SimpleForkGraph>>>,
    /// A handle to the globally shared state of the latest block.
    latest_block: LatestBlock,
    /// A shared atomic counter for ordering transactions within a single slot.
    index: Arc<AtomicUsize>,
}

impl TransactionScheduler {
    /// Creates and initializes a new `TransactionScheduler` and its associated pool of workers.
    ///
    /// This function performs the initial setup for the entire transaction processing pipeline:
    /// 1.  Prepares the shared program cache and ensures necessary sysvars are in the `AccountsDb`.
    /// 2.  Creates a pool of `TransactionExecutor` workers, each with its own dedicated channel.
    /// 3.  Spawns each worker in its own OS thread for maximum isolation and performance.
    pub fn new(executors: u32, state: TransactionSchedulerState) -> Self {
        let count = executors.max(1).min(MAX_SVM_EXECUTORS) as usize;
        let index = Arc::new(AtomicUsize::new(0));
        let mut executors = Vec::with_capacity(count);

        // Create the back-channel for workers to signal their readiness.
        let (ready_tx, ready_rx) = channel(count);
        // Perform one-time setup of the shared program cache and sysvars.
        let program_cache = state.prepare_programs_cache();
        state.prepare_sysvars();

        for id in 0..count {
            let (transactions_tx, transactions_rx) =
                channel(EXECUTOR_QUEUE_CAPACITY);
            let executor = TransactionExecutor::new(
                id as u32,
                &state,
                transactions_rx,
                ready_tx.clone(),
                index.clone(),
                program_cache.clone(),
            );
            executor.populate_builtins();
            executor.spawn();
            executors.push(transactions_tx);
        }
        let coordinator = ExecutionCoordinator::new(count);
        Self {
            coordinator,
            transactions_rx: state.txn_to_process_rx,
            ready_rx,
            executors,
            latest_block: state.ledger.latest_block().clone(),
            program_cache,
            index,
        }
    }

    /// Spawns the scheduler's main event loop into a new, dedicated OS thread.
    ///
    /// The scheduler runs in its own thread with a dedicated single-threaded Tokio
    /// runtime. This design ensures that the scheduling logic, which is a critical
    /// path, does not compete for resources with other tasks.
    pub fn spawn(self) {
        let task = move || {
            let runtime = Builder::new_current_thread()
                .thread_name("transaction-scheduler")
                .build()
                .expect("Failed to build single-threaded Tokio runtime");
            runtime.block_on(tokio::task::unconstrained(self.run()));
        };
        std::thread::spawn(task);
    }

    /// The main event loop of the transaction scheduler.
    ///
    /// This loop multiplexes between three primary events using `tokio::select!`:
    /// 1.  **Worker Readiness**: A worker signals it is ready for a new task.
    /// 2.  **New Transaction**: A new transaction arrives for processing.
    /// 3.  **New Block**: A new block is produced, triggering a slot transition.
    ///
    /// The `biased` selection ensures that ready workers are processed before
    /// the incoming transactions, which helps to keep the pipeline full and
    /// maximize throughput.
    async fn run(mut self) {
        let mut block_produced = self.latest_block.subscribe();
        loop {
            tokio::select! {
                biased;
                // A new block has been produced.
                Ok(()) = block_produced.recv() => {
                    self.transition_to_new_slot();
                }
                // A worker has finished its task and is ready for more.
                Some(executor) = self.ready_rx.recv() => {
                    self.handle_ready_executor(executor).await;
                }
                // Receive new transactions for scheduling, but
                // only if there is at least one ready worker.
                Some(txn) = self.transactions_rx.recv(), if self.coordinator.is_ready() => {
                    self.handle_new_transaction(txn).await;
                }
                // The main transaction channel has closed, indicating a system shutdown.
                else => {
                    break
                }
            }
        }
        info!("Transaction scheduler has terminated");
    }

    /// Handles a notification that a worker has become ready.
    async fn handle_ready_executor(&mut self, executor: ExecutorId) {
        self.coordinator.unlock_accounts(executor);
        self.reschedule_blocked_transactions(executor).await;
    }

    /// Handles a new transaction from the global queue.
    async fn handle_new_transaction(&mut self, txn: ProcessableTransaction) {
        // SAFETY:
        // This unwrap is safe due to the `if self.coordinator.is_ready()`
        // guard in the `select!` macro, which calls this method
        let executor = self
            .coordinator
            .get_ready_executor()
            .expect("unreachable code, if there are not any ready executors");
        let txn = TransactionWithId::new(txn);
        self.schedule_transaction(executor, txn, false).await;
    }

    /// Updates the scheduler's state when a new slot begins.
    fn transition_to_new_slot(&self) {
        // Reset the intra-slot transaction index to zero.
        self.index.store(0, std::sync::atomic::Ordering::Relaxed);
        // Re-root the shared program cache to the new slot.
        self.program_cache.write().unwrap().latest_root_slot =
            self.latest_block.load().slot;
    }

    /// Attempts to reschedule transactions that were blocked by the newly freed executor.
    async fn reschedule_blocked_transactions(&mut self, blocker: ExecutorId) {
        let mut executor = Some(blocker);
        while let Some(exec) = executor {
            let txn = self.coordinator.next_blocked_transaction(blocker);
            let scheduled = if let Some(txn) = txn {
                executor = self.coordinator.get_ready_executor();
                self.schedule_transaction(exec, txn, true).await
            } else {
                self.coordinator.release_executor(exec);
                break;
            };
            // If we failed to schedule, that means we have hit a locking snag,
            // and as we disallow frontrunning for now, we just wait for the
            // locks to be released and retry in the next scheduling cycle
            if !scheduled {
                break;
            }
        }
    }

    /// Attempts to schedule a single transaction for execution.
    ///
    /// If the transaction's required account locks are acquired, it is sent to the
    /// specified executor. Otherwise, it is queued and will be retried later.
    async fn schedule_transaction(
        &mut self,
        executor: ExecutorId,
        txn: TransactionWithId,
        prioritize: bool,
    ) -> bool {
        if let Err(blocker) = self.coordinator.try_acquire_locks(executor, &txn)
        {
            self.coordinator.release_executor(executor);
            self.coordinator.queue_transaction(blocker, txn, prioritize);
            return false;
        }
        // It's safe to ignore the result of the send operation. If the send fails,
        // it means the executor's channel is closed, which only happens on shutdown.
        let _ = self.executors[executor as usize].send(txn.txn).await;
        true
    }
}

pub mod coordinator;
pub mod locks;
pub mod state;
#[cfg(test)]
mod tests;

// SAFETY:
// Rc<RefCell> used within the scheduler never escapes to other threads
unsafe impl Send for TransactionScheduler {}
