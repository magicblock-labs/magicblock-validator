use std::sync::{atomic::AtomicUsize, Arc};

use magicblock_accounts_db::AccountsDb;
use magicblock_core::link::{
    transactions::{TxnToProcessRx, TxnToProcessTx},
    ValidatorChannelEndpoints,
};
use magicblock_ledger::{LatestBlock, Ledger};
use solana_svm::transaction_processor::TransactionProcessingEnvironment;
use tokio::sync::mpsc::{channel, Receiver};

use crate::{executor::TransactionExecutor, WorkerId};

pub struct TransactionScheduler<E> {
    transactions_rx: TxnToProcessRx,
    ready_rx: Receiver<WorkerId>,
    executors: Vec<E>,
}

pub struct TransactionSchedulerState {
    pub accountsdb: Arc<AccountsDb>,
    pub ledger: Arc<Ledger>,
    pub block: LatestBlock,
    pub environment: TransactionProcessingEnvironment<'static>,
    pub channels: ValidatorChannelEndpoints,
}

impl TransactionScheduler<(TransactionExecutor, TxnToProcessTx)> {
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
            executors.push((executor, transactions_tx));
        }
        Self {
            transactions_rx: state.channels.processable_txn_rx,
            ready_rx,
            executors,
        }
    }

    fn init(self) -> TransactionScheduler<TxnToProcessTx> {
        todo!()
    }
}

impl TransactionScheduler<TxnToProcessTx> {
    fn run(self) {}
}
