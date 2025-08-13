use std::sync::{atomic::AtomicUsize, Arc};

use magicblock_accounts_db::AccountsDb;
use magicblock_core::link::{
    accounts::AccountUpdateTx,
    transactions::{
        ProcessableTransaction, TransactionStatusTx, TransactionToProcessRx,
    },
};
use magicblock_ledger::{LatestBlock, Ledger};
use solana_svm::transaction_processor::TransactionProcessingEnvironment;
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

pub struct TransactionSchedulerState {
    pub accountsdb: Arc<AccountsDb>,
    pub ledger: Arc<Ledger>,
    pub latest_block: LatestBlock,
    pub environment: TransactionProcessingEnvironment<'static>,
    pub txn_to_process_rx: TransactionToProcessRx,
    pub account_update_tx: AccountUpdateTx,
    pub transaction_status_tx: TransactionStatusTx,
}

impl TransactionScheduler {
    pub fn new(workers: u8, state: TransactionSchedulerState) -> Self {
        let index = Arc::new(AtomicUsize::new(0));
        let mut executors = Vec::with_capacity(workers as usize);

        let (ready_tx, ready_rx) = channel(workers as usize);
        for id in 0..workers {
            let (transactions_tx, transactions_rx) = channel(1);
            let executor = TransactionExecutor::new(
                id,
                &state,
                transactions_rx,
                ready_tx.clone(),
                index.clone(),
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
                Some(txn) = self.transactions_rx.recv() => {
                    let Some(tx) = self.executors.first() else {
                        continue;
                    };
                    let _ = tx.send(txn).await;
                }
                Some(_) = self.ready_rx.recv() => {
                    // TODO use the branch with the multithreaded scheduler
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
    }
}
