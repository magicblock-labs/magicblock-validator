use std::sync::{atomic::AtomicUsize, Arc, RwLock};

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

use crate::{
    executor::{SimpleForkGraph, TransactionExecutor},
    WorkerId,
};

/// The central transaction scheduler responsible for distributing work to a
/// pool of `TransactionExecutor` workers.
///
/// This struct acts as the single entry point for all transactions entering the processing
/// pipeline. It receives transactions from a global queue and dispatches them to available
/// worker threads for execution or simulation.
pub struct TransactionScheduler {
    /// The receiving end of the global queue for all new transactions.
    transactions_rx: TransactionToProcessRx,
    /// A channel that receives readiness notifications from workers,
    /// indicating they are free to accept new work.
    ready_rx: Receiver<WorkerId>,
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
    pub fn new(workers: u8, state: TransactionSchedulerState) -> Self {
        let index = Arc::new(AtomicUsize::new(0));
        let mut executors = Vec::with_capacity(workers as usize);

        // Create the back-channel for workers to signal their readiness.
        let (ready_tx, ready_rx) = channel(workers as usize);
        // Perform one-time setup of the shared program cache and sysvars.
        let program_cache = state.prepare_programs_cache();
        state.prepare_sysvars();

        for id in 0..workers {
            // Each executor has a channel capacity of 1, as it
            // can only process one transaction at a time.
            let (transactions_tx, transactions_rx) = channel(1);
            let executor = TransactionExecutor::new(
                id,
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
        Self {
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
    /// Similar to the executors, the scheduler runs in its own thread with a dedicated
    /// single-threaded Tokio runtime for performance and to prevent it from interfering
    /// with other application tasks.
    pub fn spawn(self) {
        let task = move || {
            let runtime = Builder::new_current_thread()
                .thread_name("transaction scheduler")
                .build()
                .expect(
                    "building single threaded tokio runtime should succeed",
                );
            runtime.block_on(tokio::task::unconstrained(self.run()));
        };
        std::thread::spawn(task);
    }

    /// The main event loop of the transaction scheduler.
    ///
    /// This loop multiplexes between three primary events:
    /// 1.  Receiving a new transaction and dispatching it to an available worker.
    /// 2.  Receiving a readiness notification from a worker.
    /// 3.  Receiving a notification of a new block, triggering a slot transition.
    async fn run(mut self) {
        let mut block_produced = self.latest_block.subscribe();
        loop {
            tokio::select! {
                // Prioritize receiving new transactions.
                biased;
                Some(txn) = self.transactions_rx.recv() => {
                    // TODO(bmuddha):
                    // The current implementation sends to the first worker only.
                    // A future implementation with account-level locking will enable
                    // dispatching to any available worker.
                    let Some(tx) = self.executors.first() else {
                        continue;
                    };
                    let _ = tx.send(txn).await;
                }
                // A worker has finished its task and is ready for more.
                Some(_) = self.ready_rx.recv() => {
                    // TODO(bmuddha):
                    // This branch will be used by a multi-threaded scheduler
                    // with account-level locking to manage the pool of ready workers.
                }
                // A new block has been produced.
                _ = block_produced.recv() => {
                    self.transition_to_new_slot();
                }
                // The main transaction channel has closed, indicating a system shutdown.
                else => {
                    break
                }
            }
        }
        info!("transaction scheduler has terminated");
    }

    /// Updates the scheduler's state when a new slot begins.
    fn transition_to_new_slot(&self) {
        // Reset the intra-slot transaction index to zero.
        self.index.store(0, std::sync::atomic::Ordering::Relaxed);
        // Re-root the shared program cache to the new slot.
        self.program_cache.write().unwrap().latest_root_slot =
            self.latest_block.load().slot;
    }
}

pub mod state;
