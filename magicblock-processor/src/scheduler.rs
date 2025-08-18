use std::sync::{atomic::AtomicUsize, Arc};

use log::info;
use magicblock_core::link::transactions::{
    ProcessableTransaction, TransactionToProcessRx,
};
use magicblock_ledger::LatestBlock;
use state::TransactionSchedulerState;
use tokio::{
    runtime::Builder,
    sync::mpsc::{channel, Receiver, Sender},
};

use crate::{executor::TransactionExecutor, WorkerId};

pub struct TransactionScheduler {
    transactions_rx: TransactionToProcessRx,
    ready_rx: Receiver<WorkerId>,
    executors: Vec<Sender<ProcessableTransaction>>,
    latest_block: LatestBlock,
    index: Arc<AtomicUsize>,
}

impl TransactionScheduler {
    pub fn new(workers: u8, state: TransactionSchedulerState) -> Self {
        let index = Arc::new(AtomicUsize::new(0));
        let mut executors = Vec::with_capacity(workers as usize);

        let (ready_tx, ready_rx) = channel(workers as usize);
        let program_cache = state.prepare_programs_cache();
        state.prepare_sysvars();

        for id in 0..workers {
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
            latest_block: state.latest_block,
            index,
        }
    }

    pub fn spawn(self) {
        // For performance reasons, we need to ensure that the scheduler operates within
        // its own OS thread, but at the same time it needs some concurrency support,
        // which is why we spawn it with a dedicated single threaded tokio runtime
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

    async fn run(mut self) {
        loop {
            tokio::select! {
                // new transactions to execute or simulate, the
                // source can be any code throughout the validator
                biased; Some(txn) = self.transactions_rx.recv() => {
                    // right now we always have a single executor available,
                    // the else branch is there to avoid panicking unwraps
                    let Some(tx) = self.executors.first() else {
                        continue;
                    };
                    let _ = tx.send(txn).await;
                }
                // a back channel from executors, used to indicate that they are ready for more work
                Some(_) = self.ready_rx.recv() => {
                    // TODO(bmuddha): use the branch with the multithreaded
                    // scheduler, when account level locking is implemented
                }
                _ = self.latest_block.changed() => {
                    // when a new block/slot starts, reset the transaction index
                    self.index.store(0, std::sync::atomic::Ordering::Relaxed);
                }
                else => {
                    // transactions channel has been closed, the system is shutting down
                    break
                }
            }
        }
        info!("transaction scheduler has terminated");
    }
}

pub mod state;
