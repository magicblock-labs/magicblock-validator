//! Central transaction scheduler and event loop.
//!
//! # Architecture
//!
//! - Receives transactions from a global queue
//! - Spawns N `TransactionExecutor` workers (one per OS thread)
//! - Dispatches transactions using account locking to prevent conflicts
//! - Multiplexes between: new transactions, executor readiness, and slot transitions

use std::{
    sync::{Arc, RwLock},
    thread::JoinHandle,
};

use coordinator::{ExecutionCoordinator, TransactionWithId};
use locks::{ExecutorId, MAX_SVM_EXECUTORS};
use magicblock_accounts_db::{traits::AccountsBank, AccountsDb};
use magicblock_core::{
    link::{
        replication::{self, Message},
        transactions::{
            ProcessableTransaction, SchedulerMode, TransactionProcessingMode,
            TransactionToProcessRx,
        },
    },
    Slot,
};
use magicblock_ledger::LatestBlock;
use solana_account::{from_account, to_account};
use solana_program::slot_hashes::SlotHashes;
use solana_program_runtime::loaded_programs::ProgramCache;
use solana_sdk_ids::sysvar::{clock, slot_hashes};
use state::TransactionSchedulerState;
use tokio::{
    runtime::Builder,
    sync::{
        mpsc::{channel, Receiver, Sender},
        OwnedSemaphorePermit, Semaphore,
    },
};
use tokio_util::sync::CancellationToken;
use tracing::{error, info, instrument, warn};

use crate::executor::{
    IndexedTransaction, SimpleForkGraph, TransactionExecutor,
};

// Capacity of 1 ensures executor processes one transaction at a time
const EXECUTOR_QUEUE_CAPACITY: usize = 1;

/// Central transaction scheduler managing executor workers and transaction dispatch.
///
/// Runs in a dedicated thread with a single-threaded Tokio runtime.
pub struct TransactionScheduler {
    /// Manages executor pool, account locking, and coordination mode
    coordinator: ExecutionCoordinator,
    /// Incoming transaction queue from global processor
    transactions_rx: TransactionToProcessRx,
    /// Executor readiness notifications (workers signal when idle)
    ready_rx: Receiver<ExecutorId>,
    /// Sender channels to each executor worker
    executors: Vec<Sender<IndexedTransaction>>,
    /// Shared BPF program cache
    program_cache: Arc<RwLock<ProgramCache<SimpleForkGraph>>>,
    /// Accounts database (for sysvar updates on slot transition)
    accountsdb: Arc<AccountsDb>,
    /// Latest block metadata (slot, clock, blockhash)
    latest_block: LatestBlock,
    /// Global shutdown signal.
    shutdown: CancellationToken,
    /// Receives mode transition commands (Primary or Replica) at runtime.
    mode_rx: Receiver<SchedulerMode>,
    /// A sink for the events (transactions, blocks etc) that need to be replicated
    replication_tx: Sender<Message>,
    /// Semaphore for coordinating exclusive DB access with external callers.
    /// Scheduler acquires permit when scheduling, releases when idle.
    pause_permit: Arc<Semaphore>,
    /// Current Slot that scheduler is operating on (clock value)
    slot: Slot,
    /// Current transaction index included into the block under assembly
    index: u32,
}

impl TransactionScheduler {
    /// Creates a new scheduler and spawns its executor worker pool.
    ///
    /// Prepares shared program cache and sysvars, then spawns N executors
    /// (one per OS thread, up to MAX_SVM_EXECUTORS).
    pub fn new(executors: u32, state: TransactionSchedulerState) -> Self {
        let count = executors.clamp(1, MAX_SVM_EXECUTORS) as usize;
        let mut executors = Vec::with_capacity(count);

        // Channel for workers to signal when ready for new transactions
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
            mode_rx: state.mode_rx,
            replication_tx: state.replication_tx,
            pause_permit: state.pause_permit,
            slot: state.ledger.latest_block().load().slot,
            index: 0,
        }
    }

    /// Spawns the scheduler's event loop in a dedicated OS thread.
    pub fn spawn(self) -> JoinHandle<()> {
        std::thread::spawn(move || {
            // Single-threaded runtime avoids scheduler contention with other async tasks
            let runtime = Builder::new_current_thread()
                .thread_name("transaction-scheduler")
                .enable_all()
                .build()
                .expect("Failed to build single-threaded Tokio runtime");
            runtime.block_on(tokio::task::unconstrained(self.run()));
        })
    }

    /// Main event loop: processes executor readiness, new transactions, and slot transitions.
    ///
    /// Uses `biased` select to prioritize in this order:
    /// 1. Slot transitions
    /// 2. Executor readiness
    /// 3. Mode switches (before transactions to avoid race condition)
    /// 4. New transactions
    #[instrument(skip(self))]
    async fn run(mut self) {
        let mut block_produced = self.latest_block.subscribe();
        // Holds the scheduling permit while transactions are being processed.
        // Released when idle so external callers can acquire exclusive access.
        let mut scheduling_permit = None;
        loop {
            tokio::select! {
                biased;
                Ok(()) = block_produced.recv() => self.transition_to_new_slot(),
                Some(executor) = self.ready_rx.recv() => {
                    self.handle_ready_executor(executor).await;
                    // Release permit when idle: no active work, safe for external access
                    if self.coordinator.is_idle() {
                        scheduling_permit.take();
                    }
                }
                Some(mode) = self.mode_rx.recv() => {
                    match mode {
                        SchedulerMode::Primary => {
                            self.coordinator.switch_to_primary_mode_globally();
                        }
                        SchedulerMode::Replica => {
                            self.coordinator.switch_to_replica_mode_globally();
                        }
                    }
                }
                Some(txn) = self.transactions_rx.recv(), if self.coordinator.is_ready() => {
                    self.handle_new_transaction(txn, &mut scheduling_permit).await;
                }
                _ = self.shutdown.cancelled() => break,
                else => break,
            }
        }
        // Shutdown: drop executor channels to signal workers to stop,
        // then drain remaining ready notifications
        drop(self.executors);
        while self.ready_rx.recv().await.is_some() {}
        info!("Scheduler terminated");
    }

    async fn handle_ready_executor(&mut self, executor: ExecutorId) {
        self.coordinator.unlock_accounts(executor);
        self.reschedule_blocked_transactions(executor).await;
    }

    async fn handle_new_transaction(
        &mut self,
        txn: ProcessableTransaction,
        scheduling_permit: &mut Option<OwnedSemaphorePermit>,
    ) {
        if !self.coordinator.is_transaction_allowed(&txn.mode) {
            warn!("Dropping transaction due to mode incompatibility");
            return;
        }
        // Acquire permit if not already held. This blocks if an external caller
        // (e.g., checksum) currently holds it, ensuring mutual exclusion.
        if scheduling_permit.is_none() {
            let permit = self
                .pause_permit
                .clone()
                .acquire_owned()
                .await
                .expect("scheduler semaphore can never be closed");
            scheduling_permit.replace(permit);
        }
        // SAFETY:
        // the caller ensured that executor was ready before invoking this
        // method so the get_ready_executor should always return Some here
        let executor = self.coordinator.get_ready_executor().expect(
            "unreachable: is_ready() guard ensures an executor is available",
        );
        self.schedule_transaction(executor, TransactionWithId::new(txn))
            .await;
    }

    async fn reschedule_blocked_transactions(&mut self, blocker: ExecutorId) {
        let mut executor = Some(blocker);
        while let Some(exec) = executor.take() {
            // Try to get next transaction blocked by this executor
            let Some(txn) = self.coordinator.next_blocked_transaction(blocker)
            else {
                // Queue empty: release executor back to pool
                self.coordinator.release_executor(exec);
                break;
            };

            let blocked = self.schedule_transaction(exec, txn).await;

            // If blocked by the same executor we're draining, stop to avoid infinite loop
            if blocked.is_some_and(|b| b == blocker) {
                break;
            }
            // Try to get another executor for the next blocked transaction
            executor = self.coordinator.get_ready_executor();
        }
    }

    async fn schedule_transaction(
        &mut self,
        executor: ExecutorId,
        txn: TransactionWithId,
    ) -> Option<ExecutorId> {
        let txn = match self.coordinator.try_schedule(executor, txn) {
            Ok(txn) => txn,
            Err(blocker) => return Some(blocker),
        };
        let (slot, index) =
            if let TransactionProcessingMode::Replay(ctx) = txn.mode {
                (ctx.slot, ctx.index)
            } else {
                let index = self.index;
                self.index += 1;
                (self.slot, index)
            };

        let msg = txn
            .encoded
            .as_ref()
            .cloned()
            .filter(|_| {
                matches!(txn.mode, TransactionProcessingMode::Execution(_))
                    && self.coordinator.should_replicate()
            })
            .map(|payload| {
                Message::Transaction(replication::Transaction {
                    index,
                    slot,
                    payload,
                })
            });
        let txn = IndexedTransaction { slot, index, txn };

        let sent = self.executors[executor as usize].try_send(txn).inspect_err(
            |error| error!(executor, %error, "Executor channel send failed"),
        ).is_ok();
        if let Some(msg) = msg.filter(|_| sent) {
            let _ = self.replication_tx.send(msg).await.inspect_err(
                |error| error!(executor, %error, "Replication channel send failed"),
            );
        }
        None
    }

    fn transition_to_new_slot(&mut self) {
        let block = self.latest_block.load();
        let mut cache = self.program_cache.write().unwrap();
        self.slot = block.slot;
        self.index = 0;

        // Prune stale programs and re-root to new slot
        cache.prune(block.slot, 0);
        cache.latest_root_slot = block.slot;

        // Update Clock sysvar
        if let Some(mut account) = self.accountsdb.get_account(&clock::ID) {
            let _ = account.serialize_data(&block.clock);
            let _ = self.accountsdb.insert_account(&clock::ID, &account);
        }

        // Update SlotHashes sysvar
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
        // Release lock before syscall lookup (prevents deadlock if sysvar is accessed)
        drop(cache);
    }
}

// SAFETY:
// Rc<RefCell> used in the the scheduler is only used in a single
// thread, the scheduler is meant to stay single threaded
unsafe impl Send for TransactionScheduler {}

pub mod coordinator;
pub mod locks;
pub mod state;
#[cfg(test)]
mod tests;
