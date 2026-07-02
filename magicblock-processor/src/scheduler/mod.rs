//! Central transaction scheduler and event loop.
//!
//! # Architecture
//!
//! - Receives transactions from a global queue
//! - Spawns N `TransactionExecutor` workers (one per OS thread)
//! - Dispatches transactions using account locking to prevent conflicts
//! - Multiplexes between: new transactions, executor readiness, and slot transitions
//!
//! # Streaming Blockhash Algorithm
//!
//! Block hashes are computed incrementally as transactions are processed, rather than
//! waiting until the end of a slot. This enables:
//!
//! - **Early detection of hash divergence** between primary and replica nodes
//! - **Reduced latency** for block finalization
//!
//! The algorithm works as follows:
//! 1. At slot boundary, the hasher is reset and seeded with the previous blockhash
//! 2. Each transaction signature is hashed into the stream as it's scheduled
//! 3. At slot completion, the accumulated hash becomes the new blockhash
//!
//! # Primary vs Replica Responsibilities
//!
//! ## Primary Mode
//!
//! - Acts as the clock source, driving slot transitions via `slot_ticker`
//! - Computes blockhash and broadcasts it via replication channel
//! - Writes completed blocks to the ledger
//!
//! ## Replica Mode
//!
//! - Waits for block production notifications from replication service
//! - Verifies computed blockhash matches the received blockhash
//! - Logs divergence errors (does not halt execution)
//!
//! # Slot Transition Lifecycle
//!
//! 1. **Finalize current block** - compute blockhash, persist to ledger
//! 2. **Broadcast block** - send to replication channel (primary only)
//! 3. **Reset hasher** - seed with previous blockhash for next slot
//! 4. **Advance slot** - increment slot number, reset transaction index
//! 5. **Update program cache** - prune stale programs, re-root to new slot
//! 6. **Update sysvars** - Clock and SlotHashes accounts

use core::matches;
use std::{
    sync::{Arc, RwLock},
    thread::JoinHandle,
    time::{SystemTime, UNIX_EPOCH},
};

use blake3::Hasher;
use coordinator::{ExecutionCoordinator, TransactionWithId};
use locks::{ExecutorId, MAX_SVM_EXECUTORS};
use magicblock_accounts_db::{traits::AccountsBank, AccountsDb};
use magicblock_core::{
    link::{
        replication::{self, Message, SuperBlock},
        transactions::{
            ProcessableTransaction, SchedulerCommand, SchedulerCommandResult,
            SchedulerMode, TransactionProcessingMode, TransactionToProcessRx,
        },
    },
    Slot,
};
use magicblock_ledger::{LatestBlock, LatestBlockInner, Ledger};
use magicblock_metrics::metrics;
use magicblock_program::sysvar::{HighPrecisionClock, HIGH_PRECISION_CLOCK_ID};
use solana_account::{from_account, to_account};
use solana_program::{clock::Clock, hash::Hash, slot_hashes::SlotHashes};
use solana_program_runtime::loaded_programs::ProgramCache;
use solana_sdk_ids::sysvar::{clock, slot_hashes};
use state::TransactionSchedulerState;
use tokio::{
    runtime::Builder,
    sync::{
        mpsc::{channel, Receiver, Sender},
        oneshot, OwnedSemaphorePermit, Semaphore,
    },
    time::{interval, Interval},
};
use tokio_util::sync::CancellationToken;
use tracing::{error, info, instrument, warn};

use crate::executor::{
    ExecutorCommand, IndexedTransaction, SimpleForkGraph, TransactionExecutor,
};

// Capacity of 1 ensures executor processes one transaction at a time
const EXECUTOR_QUEUE_CAPACITY: usize = 1;

/// Central transaction scheduler managing executor workers and transaction dispatch.
///
/// Runs in a dedicated thread with a single-threaded Tokio runtime.
pub struct TransactionScheduler {
    /// Manages executor pool, account locking, and coordination mode
    coordinator: ExecutionCoordinator,
    /// Ordered command queue from global processor
    transactions_rx: TransactionToProcessRx,
    /// Executor readiness notifications (workers signal when idle)
    ready_rx: Receiver<ExecutorId>,
    /// Sender channels to each executor worker
    executors: Vec<Sender<ExecutorCommand>>,
    /// Shared BPF program cache
    program_cache: Arc<RwLock<ProgramCache<SimpleForkGraph>>>,
    /// Accounts database (for sysvar updates on slot transition)
    accountsdb: Arc<AccountsDb>,
    /// Global transactions ledger
    ledger: Arc<Ledger>,
    /// Global shutdown signal.
    shutdown: CancellationToken,
    /// Receives mode transition commands (Primary or Replica) at runtime.
    mode_rx: Receiver<SchedulerMode>,
    /// A sink for the events (transactions, blocks etc) that need to be replicated
    replication_tx: Sender<Message>,
    /// Semaphore for coordinating exclusive DB access with external callers.
    /// Scheduler acquires permit when scheduling, releases when idle.
    pause_permit: Arc<Semaphore>,
    /// Streaming blockhash state
    hasher: Hasher,
    /// Time interval between consecutive slots
    slot_ticker: Interval,
    /// Number of slots to elapse, before we perform snapshot/checksum operations
    superblock_size: u64,
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

        let execution_permits = Arc::new(Semaphore::new(count));
        for id in 0..count {
            let (transactions_tx, transactions_rx) =
                channel(EXECUTOR_QUEUE_CAPACITY);
            let executor = TransactionExecutor::new(
                id as u32,
                &state,
                transactions_rx,
                ready_tx.clone(),
                execution_permits.clone(),
                program_cache.clone(),
            );
            executor.populate_builtins();
            executor.spawn();
            executors.push(transactions_tx);
        }

        let mut hasher = Hasher::new();
        let slot_ticker = interval(state.block_time);
        let latest_block = state.ledger.latest_block().clone();
        hasher.update(
            Self::initial_blockhash(&state.accountsdb, &latest_block).as_ref(),
        );
        let slot = state.accountsdb.slot() + 1;

        Self {
            coordinator: ExecutionCoordinator::new(count, execution_permits),
            transactions_rx: state.txn_to_process_rx,
            ready_rx,
            executors,
            ledger: state.ledger,
            program_cache,
            accountsdb: state.accountsdb,
            shutdown: state.shutdown,
            mode_rx: state.mode_rx,
            replication_tx: state.replication_tx,
            pause_permit: state.pause_permit,
            superblock_size: state.superblock_size,
            hasher,
            slot_ticker,
            slot,
            index: 0,
        }
    }

    fn initial_blockhash(
        accountsdb: &AccountsDb,
        latest_block: &LatestBlock,
    ) -> Hash {
        let ledger_hash = latest_block.load().blockhash;
        if ledger_hash != Hash::default() {
            return ledger_hash;
        }

        let snapshot_hash = accountsdb
            .get_account(&slot_hashes::ID)
            .and_then(|account| from_account::<SlotHashes, _>(&account))
            .and_then(|hashes| {
                hashes.slot_hashes().first().map(|(_, hash)| *hash)
            });
        if let Some(hash) = snapshot_hash {
            return hash;
        }

        warn!("initial blockhash was not found, starting with an empty one");

        Hash::default()
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
        // Holds the scheduling permit while transactions are being processed.
        // Released when idle so external callers can acquire exclusive access.
        let mut scheduling_permit = None;
        let mut pending_command = None;
        loop {
            if let Some(command) = pending_command.take() {
                match command {
                    SchedulerCommand::Transaction(txn) => {
                        if self.coordinator.is_ready() {
                            self.handle_new_transaction(
                                txn,
                                &mut scheduling_permit,
                            )
                            .await;
                            continue;
                        }
                        pending_command =
                            Some(SchedulerCommand::Transaction(txn));
                    }
                    SchedulerCommand::Block { block, ack } => {
                        if self.coordinator.is_idle() {
                            self.handle_replayed_block(block, ack).await;
                            continue;
                        }
                        pending_command =
                            Some(SchedulerCommand::Block { block, ack });
                    }
                    SchedulerCommand::Drain { ack } => {
                        if self.coordinator.is_idle() {
                            let _ = ack.send(Ok(()));
                            continue;
                        }
                        pending_command = Some(SchedulerCommand::Drain { ack });
                    }
                }
            }
            tokio::select! {
                biased;
                _ = self.slot_ticker.tick() => {
                    if self.coordinator.is_primary() {
                        let slot = self.transition_to_new_slot(None).await;
                        self.handle_superblock(slot).await;
                    }
                }
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
                Some(command) = self.transactions_rx.recv(), if pending_command.is_none() && self.coordinator.is_ready() => {
                    pending_command = Some(command);
                }
                _ = self.shutdown.cancelled() => break,
                else => break,
            }
        }
        if self.coordinator.is_primary() {
            let slot = self.transition_to_new_slot(None).await;
            self.handle_superblock(slot).await;
        }
        // Shutdown: drop executor channels to signal workers to stop,
        // then drain remaining ready notifications
        drop(self.executors);
        while self.ready_rx.recv().await.is_some() {}
        info!("Scheduler terminated");
    }

    /// Sends a replication message, logging any errors.
    async fn send_replication(&self, msg: Message) {
        if self.replication_tx.is_closed() {
            return;
        }
        let kind = msg.kind();
        if let Err(error) = self.replication_tx.send(msg).await {
            error!(
                %error,
                kind,
                slot = self.slot,
                index = self.index,
                "replication send failed"
            );
        }
    }

    async fn handle_superblock(&mut self, slot: Slot) {
        if !slot.is_multiple_of(self.superblock_size) {
            return;
        }

        // Wait until the scheduler has no assigned or executing work left,
        // then freeze executor starts while snapshotting.
        let _guard = self.pause_executors_for_snapshot().await;
        // SAFETY:
        // we have made sure that no state transitions are in progress via _guard
        let Ok(checksum) = (unsafe { self.accountsdb.take_snapshot(slot) })
        else {
            error!("failed to create accountsdb snapshot");
            return;
        };
        if self.coordinator.is_primary() {
            let msg = Message::SuperBlock(SuperBlock { slot, checksum });
            self.send_replication(msg).await;
        }
    }

    async fn pause_executors_for_snapshot(&mut self) -> OwnedSemaphorePermit {
        while !self.coordinator.is_idle() {
            if let Some(executor) = self.ready_rx.recv().await {
                self.handle_ready_executor(executor).await;
            }
        }

        self.coordinator.wait_for_idle().await
    }

    async fn handle_ready_executor(&mut self, executor: ExecutorId) {
        self.coordinator.unlock_accounts(executor);
        self.reschedule_blocked_transactions(executor).await;
    }

    async fn handle_replayed_block(
        &mut self,
        block: replication::Block,
        ack: oneshot::Sender<SchedulerCommandResult>,
    ) {
        let result = self.apply_replayed_block(block).await;
        let _ = ack.send(result);
    }

    async fn apply_replayed_block(
        &mut self,
        block: replication::Block,
    ) -> SchedulerCommandResult {
        if self.coordinator.is_primary() {
            return Err(
                "replicated block received while in primary mode".into()
            );
        }

        let permit = self
            .pause_permit
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| "scheduler semaphore closed".to_string())?;

        let block = LatestBlockInner::new_with_nanos(
            block.slot,
            block.hash,
            block.timestamp,
            block.nanos,
        );
        self.verify_block_as_replica(&block);
        self.ledger
            .write_block(block.clone())
            .map_err(|error| error.to_string())?;
        self.accountsdb.set_slot(block.slot);
        self.update_sysvars(&block);
        let slot = block.slot;
        let result = self.notify_executors_of_block(block).await;
        drop(permit);
        // apply_replayed_block advances self.slot before the queued
        // latest_block notification can be received. The block_produced path
        // then skips transition_to_new_slot for this already-applied slot, so
        // handle_superblock must run here for replayed superblock snapshots.
        if result.is_ok() {
            self.handle_superblock(slot).await;
        }
        result
    }

    async fn notify_executors_of_block(
        &mut self,
        block: LatestBlockInner,
    ) -> SchedulerCommandResult {
        let mut acks = Vec::with_capacity(self.executors.len());
        for (executor, tx) in self.executors.iter().enumerate() {
            let (ack, rx) = oneshot::channel();
            tx.send(ExecutorCommand::Block {
                block: block.clone(),
                ack,
            })
            .await
            .map_err(|error| {
                format!("executor {executor} block command failed: {error}")
            })?;
            acks.push((executor, rx));
        }

        for (executor, ack) in acks {
            ack.await.map_err(|_| {
                format!("executor {executor} dropped block acknowledgement")
            })?;
        }
        Ok(())
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
        let mut is_execution = false;
        let (slot, index) = match txn.mode {
            TransactionProcessingMode::Replay(ctx) => (ctx.slot, ctx.index),
            TransactionProcessingMode::Simulation(_) => (self.slot, 0),
            TransactionProcessingMode::Execution(_) => {
                is_execution = true;
                let index = self.index;
                // we only advance the index if we are executing (primary mode)
                self.index += 1;
                (self.slot, index)
            }
        };
        if !matches!(txn.mode, TransactionProcessingMode::Simulation(_)) {
            self.hasher.update(txn.transaction.signature().as_ref());
        }

        let msg = txn
            .encoded
            .as_ref()
            .cloned()
            .filter(|_| is_execution && self.coordinator.is_primary())
            .map(|payload| {
                Message::Transaction(replication::Transaction {
                    index,
                    slot,
                    payload,
                })
            });
        let txn = IndexedTransaction { slot, index, txn };

        let sent = self.executors[executor as usize]
            .try_send(ExecutorCommand::Transaction(txn))
            .inspect_err(
                |error| error!(executor, %error, "Executor channel send failed"),
            )
            .is_ok();
        if let Some(msg) = msg.filter(|_| sent) {
            self.send_replication(msg).await;
        }
        None
    }

    /// Transitions to the next slot, finalizing the current block.
    ///
    /// In primary mode, this drives the slot transition clock and broadcasts
    /// the new block. In replica mode, this responds to block notifications.
    async fn transition_to_new_slot(
        &mut self,
        block: Option<LatestBlockInner>,
    ) -> u64 {
        let block = self.prepare_block(block).await;
        self.finalize_block(block.clone()).await;
        self.update_sysvars(&block);
        metrics::set_slot(block.slot);
        block.slot
    }

    /// Prepares the block for the current slot.
    ///
    /// In primary mode: computes blockhash, creates block from local state.
    /// In replica mode: uses the block from the replication stream, verifying hash.
    async fn prepare_block(
        &mut self,
        block: Option<LatestBlockInner>,
    ) -> LatestBlockInner {
        if let Some(block) = block {
            self.verify_block_as_replica(&block);
            block
        } else {
            self.prepare_block_as_primary().await
        }
    }

    /// Prepares block as primary: computes blockhash and broadcasts to replicas.
    async fn prepare_block_as_primary(&mut self) -> LatestBlockInner {
        let blockhash = (*self.hasher.finalize().as_bytes()).into();
        // NOTE:
        // As we have a single node network, we have no
        // option but to use the time from host machine
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
        // NOTE: since we can tick very frequently, a lot
        // of blocks might have identical timestamps
        let timestamp = now.as_secs() as i64;
        // The discarded sub-second precision is preserved separately and
        // exposed via the HighPrecisionClock sysvar.
        let nanos = now.subsec_nanos();
        let block = LatestBlockInner::new_with_nanos(
            self.slot, blockhash, timestamp, nanos,
        );
        let msg = Message::Block(replication::Block {
            slot: block.slot,
            hash: block.blockhash,
            timestamp: block.clock.unix_timestamp,
            nanos: block.nanos,
        });
        self.send_replication(msg).await;
        block
    }

    /// Checks that the blockhash received from replication stream matches the local
    fn verify_block_as_replica(&self, block: &LatestBlockInner) {
        if block.blockhash.as_ref() != self.hasher.finalize().as_bytes() {
            // TODO(bmuddha):
            // this should never happen, and it's unclear how
            // to recover from it, right now the log is used
            // for debugging purposes only
            // NOTE:
            // This error will always be logged once
            // when replica starts up with an empty ledger
            error!(
                slot = block.slot,
                "replication blockhash has diverged from local"
            )
        }
    }

    /// Finalizes the block: persists to ledger and updates accountsdb slot.
    async fn finalize_block(&self, block: LatestBlockInner) {
        let slot = block.slot;
        let mut block_written = true;
        if self.coordinator.is_primary() {
            block_written = self.ledger.write_block(block).inspect_err(
                |error| error!(%error, %slot, "failed to write block to the ledger"),
            ).is_ok();
        }
        if block_written {
            self.accountsdb.set_slot(slot);
        }
    }

    /// Updates sysvars and program cache for the new slot.
    ///
    /// This must be called after block finalization to prepare state for the next slot.
    fn update_sysvars(&mut self, block: &LatestBlockInner) {
        // Reset hasher and seed with previous blockhash for next slot
        self.hasher.reset();
        self.hasher.update(block.blockhash.as_ref());

        // Advance slot and reset transaction index
        self.slot = block.clock.slot;
        self.index = 0;

        self.update_program_cache(block.slot);
        self.update_clock_sysvar(&block.clock);
        self.update_high_precision_clock_sysvar(block);
        self.update_slot_hashes_sysvar(block.slot, &block.blockhash);
    }

    /// Updates the program cache for the new slot.
    fn update_program_cache(&mut self, slot: Slot) {
        let mut cache = self.program_cache.write().unwrap();
        // Prune stale programs and re-root to new slot
        cache.prune(slot, None);
        // Release lock before syscall lookup (prevents deadlock if sysvar is accessed)
        drop(cache);
    }

    /// Updates the Clock sysvar account.
    fn update_clock_sysvar(&self, clock: &Clock) {
        if let Some(mut account) = self.accountsdb.get_account(&clock::ID) {
            let _ = account.serialize_data(clock);
            let _ = self.accountsdb.insert_account(&clock::ID, &account);
        }
    }

    /// Updates the HighPrecisionClock sysvar account.
    fn update_high_precision_clock_sysvar(&self, block: &LatestBlockInner) {
        if let Some(mut account) =
            self.accountsdb.get_account(&HIGH_PRECISION_CLOCK_ID)
        {
            let high_precision_clock = HighPrecisionClock {
                unix_timestamp: block.clock.unix_timestamp,
                nanos: block.nanos,
            };
            let _ = account.serialize_data(&high_precision_clock);
            let _ = self
                .accountsdb
                .insert_account(&HIGH_PRECISION_CLOCK_ID, &account);
        }
    }

    /// Updates the SlotHashes sysvar account with the new slot/blockhash pair.
    fn update_slot_hashes_sysvar(&self, slot: Slot, blockhash: &Hash) {
        if let Some(mut acc) = self.accountsdb.get_account(&slot_hashes::ID) {
            let Some(mut hashes) = from_account::<SlotHashes, _>(&acc) else {
                warn!("failed to read slot hashes from account");
                return;
            };
            hashes.add(slot, *blockhash);
            if to_account(&hashes, &mut acc).is_none() {
                warn!("failed to write slot hashes to account");
            }
            let _ = self.accountsdb.insert_account(&slot_hashes::ID, &acc);
        }
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
