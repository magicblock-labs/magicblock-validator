//! The central transaction scheduler and its event loop.
//!
//! This module is the entry point for all transactions into the processing pipeline.
//! It is responsible for creating and managing a pool of `TransactionExecutor`
//! workers and dispatching transactions to them for execution.

use std::{
    sync::{Arc, RwLock},
    thread::JoinHandle,
};

use coordinator::{ExecutionCoordinator, TransactionWithId};
use locks::{ExecutorId, MAX_SVM_EXECUTORS};
use magicblock_accounts_db::{traits::AccountsBank, AccountsDb};
use magicblock_core::link::transactions::{
    ProcessableTransaction, TransactionToProcessRx,
};
use magicblock_ledger::LatestBlock;
use solana_account::{from_account, to_account};
use solana_program::slot_hashes::SlotHashes;
use solana_program_runtime::loaded_programs::ProgramCache;
use solana_sdk_ids::sysvar::{clock, slot_hashes};
use state::TransactionSchedulerState;
use tokio::{
    runtime::Builder,
    sync::mpsc::{channel, Receiver, Sender},
};
use tokio_util::sync::CancellationToken;
use tracing::{error, info, instrument, warn};

use crate::executor::{SimpleForkGraph, TransactionExecutor};

const EXECUTOR_QUEUE_CAPACITY: usize = 1;

/// The central transaction scheduler responsible for distributing work to executors.
pub struct TransactionScheduler {
    coordinator: ExecutionCoordinator,
    transactions_rx: TransactionToProcessRx,
    ready_rx: Receiver<ExecutorId>,
    executors: Vec<Sender<ProcessableTransaction>>,
    program_cache: Arc<RwLock<ProgramCache<SimpleForkGraph>>>,
    accountsdb: Arc<AccountsDb>,
    latest_block: LatestBlock,
    shutdown: CancellationToken,
}

impl TransactionScheduler {
    pub fn new(executors: u32, state: TransactionSchedulerState) -> Self {
        let count = executors.clamp(1, MAX_SVM_EXECUTORS) as usize;
        let mut executors = Vec::with_capacity(count);

        let (ready_tx, ready_rx) = channel(count);
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
                program_cache.clone(),
            );
            executor.populate_builtins();
            executor.spawn();
            executors.push(transactions_tx);
        }

        Self {
            coordinator: ExecutionCoordinator::new(count),
            transactions_rx: state.txn_to_process_rx,
            ready_rx,
            executors,
            latest_block: state.ledger.latest_block().clone(),
            program_cache,
            accountsdb: state.accountsdb,
            shutdown: state.shutdown,
        }
    }

    pub fn spawn(self) -> JoinHandle<()> {
        std::thread::spawn(move || {
            let runtime = Builder::new_current_thread()
                .thread_name("transaction-scheduler")
                .build()
                .expect("Failed to build single-threaded Tokio runtime");
            runtime.block_on(tokio::task::unconstrained(self.run()));
        })
    }

    #[instrument(skip(self))]
    async fn run(mut self) {
        let mut block_produced = self.latest_block.subscribe();
        loop {
            tokio::select! {
                biased;
                Ok(()) = block_produced.recv() => self.transition_to_new_slot(),
                Some(executor) = self.ready_rx.recv() => self.handle_ready_executor(executor),
                Some(txn) = self.transactions_rx.recv(), if self.coordinator.is_ready() => {
                    self.handle_new_transaction(txn);
                }
                _ = self.shutdown.cancelled() => break,
                else => break,
            }
        }
        drop(self.executors);
        self.ready_rx.recv().await;
        info!("Scheduler terminated");
    }

    fn handle_ready_executor(&mut self, executor: ExecutorId) {
        self.coordinator.unlock_accounts(executor);
        self.reschedule_blocked_transactions(executor);
    }

    fn handle_new_transaction(&mut self, txn: ProcessableTransaction) {
        let executor = self.coordinator.get_ready_executor().expect(
            "unreachable: is_ready() guard ensures an executor is available",
        );
        self.schedule_transaction(executor, TransactionWithId::new(txn));
    }

    fn reschedule_blocked_transactions(&mut self, blocker: ExecutorId) {
        let mut executor = Some(blocker);
        while let Some(exec) = executor.take() {
            let Some(txn) = self.coordinator.next_blocked_transaction(blocker)
            else {
                self.coordinator.release_executor(exec);
                break;
            };

            let blocked = self.schedule_transaction(exec, txn);

            // If blocked by the same executor we're draining, stop to avoid infinite loop
            if blocked.is_some_and(|b| b == blocker) {
                break;
            }
            executor = self.coordinator.get_ready_executor();
        }
    }

    fn schedule_transaction(
        &mut self,
        executor: ExecutorId,
        txn: TransactionWithId,
    ) -> Option<ExecutorId> {
        let txn = match self.coordinator.try_schedule(executor, txn) {
            Ok(txn) => txn,
            Err(blocker) => return Some(blocker),
        };

        let _ = self.executors[executor as usize].try_send(txn).inspect_err(
            |e| error!(executor, error = ?e, "Executor channel send failed"),
        );
        None
    }

    fn transition_to_new_slot(&self) {
        let block = self.latest_block.load();
        let mut cache = self.program_cache.write().unwrap();
        cache.prune(block.slot, 0);
        cache.latest_root_slot = block.slot;

        if let Some(mut account) = self.accountsdb.get_account(&clock::ID) {
            let _ = account.serialize_data(&block.clock);
            let _ = self.accountsdb.insert_account(&clock::ID, &account);
        }

        if let Some(mut acc) = self.accountsdb.get_account(&slot_hashes::ID) {
            let Some(mut hashes) = from_account::<SlotHashes, _>(&acc) else {
                warn!("failed to read slot hashes from account");
                return;
            };
            hashes.add(block.slot, block.blockhash);
            if to_account(&hashes, &mut acc).is_none() {
                warn!("failed to write slot hashes to account");
            }
            let _ = self.accountsdb.insert_account(&slot_hashes::ID, &acc);
        }
        drop(cache);
    }
}

pub mod coordinator;
pub mod locks;
pub mod state;
#[cfg(test)]
mod tests;

// SAFETY: Rc<RefCell> used within the scheduler never escapes to other threads
unsafe impl Send for TransactionScheduler {}
