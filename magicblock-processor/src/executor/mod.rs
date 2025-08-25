use std::sync::{atomic::AtomicUsize, Arc, RwLock};

use log::info;
use magicblock_accounts_db::{AccountsDb, StWLock};
use magicblock_core::link::{
    accounts::AccountUpdateTx,
    transactions::{
        TransactionProcessingMode, TransactionStatusTx, TransactionToProcessRx,
    },
};
use magicblock_ledger::{LatestBlock, LatestBlockInner, Ledger};
use parking_lot::RwLockReadGuard;
use solana_program_runtime::loaded_programs::{
    BlockRelation, ForkGraph, ProgramCache, ProgramCacheEntry,
};
use solana_svm::transaction_processor::{
    ExecutionRecordingConfig, TransactionBatchProcessor,
    TransactionProcessingConfig, TransactionProcessingEnvironment,
};
use tokio::{runtime::Builder, sync::mpsc::Sender};

use crate::{
    builtins::BUILTINS, scheduler::state::TransactionSchedulerState, WorkerId,
};

/// A dedicated, single-threaded worker responsible for processing transactions using
/// the Solana SVM. This struct represents the computational core of the validator.
/// It operates in isolation, pulling transactions from a queue, executing them against
/// the current state, committing the results, and broadcasting updates. Multiple
/// executors can be spawned to process transactions in parallel.
pub(super) struct TransactionExecutor {
    /// A unique identifier for this worker instance.
    id: WorkerId,
    /// A handle to the global accounts database for reading and writing account state.
    accountsdb: Arc<AccountsDb>,
    /// A handle to the global ledger for writing committed transaction history.
    ledger: Arc<Ledger>,
    /// The core Solana SVM `TransactionBatchProcessor` that loads and executes transactions.
    processor: TransactionBatchProcessor<SimpleForkGraph>,
    /// An immutable configuration for the SVM, set at startup.
    config: Box<TransactionProcessingConfig<'static>>,
    /// A handle to the globally shared state of the latest block.
    block: LatestBlock,
    /// A reusable SVM environment for transaction processing.
    environment: TransactionProcessingEnvironment<'static>,
    /// The channel from which this worker receives new transactions to process.
    rx: TransactionToProcessRx,
    /// A channel to send out the final status of processed transactions.
    transaction_tx: TransactionStatusTx,
    /// A channel to send out account state updates after processing.
    accounts_tx: AccountUpdateTx,
    /// A back-channel to notify the `TransactionScheduler` that this worker is ready for more work.
    ready_tx: Sender<WorkerId>,
    /// A read lock held during a slot's processing to synchronize with critical global
    /// operations like `AccountsDb` snapshots.
    sync: StWLock,
    /// An atomic counter for ordering transactions within a single slot.
    index: Arc<AtomicUsize>,
}

impl TransactionExecutor {
    /// Creates a new `TransactionExecutor` worker.
    ///
    /// It initializes the SVM processor and, for performance, overrides its local program cache
    /// with a globally shared one. This allows updates made by one executor to be immediately
    /// visible to all others, preventing redundant program loads.
    pub(super) fn new(
        id: WorkerId,
        state: &TransactionSchedulerState,
        rx: TransactionToProcessRx,
        ready_tx: Sender<WorkerId>,
        index: Arc<AtomicUsize>,
        programs_cache: Arc<RwLock<ProgramCache<SimpleForkGraph>>>,
    ) -> Self {
        let slot = state.accountsdb.slot();
        let mut processor = TransactionBatchProcessor::new_uninitialized(
            slot,
            Default::default(),
        );

        // Override the default program cache with a globally shared one.
        processor.program_cache = programs_cache;

        // NOTE: Enabling full recording (as it is done here)
        // can have a noticeable performance impact.
        let recording_config =
            ExecutionRecordingConfig::new_single_setting(true);
        let config = Box::new(TransactionProcessingConfig {
            recording_config,
            ..Default::default()
        });
        let this = Self {
            id,
            sync: state.accountsdb.synchronizer(),
            processor,
            accountsdb: state.accountsdb.clone(),
            ledger: state.ledger.clone(),
            config,
            block: state.ledger.latest_block().clone(),
            environment: state.environment.clone(),
            rx,
            ready_tx,
            accounts_tx: state.account_update_tx.clone(),
            transaction_tx: state.transaction_status_tx.clone(),
            index,
        };

        this.processor.fill_missing_sysvar_cache_entries(&this);
        this
    }

    /// Registers all Solana builtin programs (e.g., System Program, BPF Loader) with the SVM.
    pub(super) fn populate_builtins(&self) {
        for program in BUILTINS {
            let entry = ProgramCacheEntry::new_builtin(
                0,
                program.name.len(),
                program.entrypoint,
            );
            self.processor.add_builtin(
                self,
                program.program_id,
                program.name,
                entry,
            );
        }
    }

    /// Spawns the transaction executor into a new, dedicated OS thread.
    ///
    /// For performance and isolation, each executor runs in its own thread
    /// with a dedicated single-threaded Tokio runtime. This avoids contention
    /// with other asynchronous tasks in the main application runtime.
    pub(super) fn spawn(self) {
        let task = move || {
            let runtime = Builder::new_current_thread()
                .thread_name(format!("transaction executor #{}", self.id))
                .build()
                .expect(
                    "building single threaded tokio runtime should succeed",
                );
            runtime.block_on(tokio::task::unconstrained(self.run()));
        };
        std::thread::spawn(task);
    }

    /// The main event loop of the transaction executor.
    ///
    /// At the start of each slot, it acquires a read lock to prevent disruptive global
    /// operations (like snapshotting) during transaction processing. This lock is
    /// released and re-acquired at every slot boundary. The loop multiplexes between
    /// processing new transactions and handling new block notifications.
    async fn run(mut self) {
        let mut _guard = self.sync.read();
        let mut block_updated = self.block.subscribe();

        loop {
            tokio::select! {
                // Prioritize processing incoming transactions.
                biased;
                Some(txn) = self.rx.recv() => {
                    match txn.mode {
                        TransactionProcessingMode::Execution(tx) => {
                            self.execute([txn.transaction], tx, false);
                        }
                        TransactionProcessingMode::Simulation(tx) => {
                            self.simulate([txn.transaction], tx);
                        }
                        TransactionProcessingMode::Replay(tx) => {
                            self.execute([txn.transaction], Some(tx), true);
                        }
                    }
                    // Notify the scheduler that this worker is ready for another transaction.
                    let _ = self.ready_tx.send(self.id).await;
                }
                // When a new block is produced, transition to the new slot.
                _ = block_updated.recv() => {
                    // Fairly release the lock to allow any pending critical operations to proceed.
                    RwLockReadGuard::unlock_fair(_guard);
                    self.transition_to_new_slot();
                    // Re-acquire the lock to begin processing for the new slot. This will block
                    // only if a critical operation (like a snapshot) is in progress.
                    _guard = self.sync.read();
                }
                // If the transaction channel closes, the system is shutting down.
                else => {
                    break;
                }
            }
        }
        info!("transaction executor {} has terminated", self.id)
    }

    /// Updates the executor's internal state to align with a new slot.
    /// This updates the SVM processor's current slot, blockhash, and relevant sysvars.
    fn transition_to_new_slot(&mut self) {
        let block = self.block.load();
        self.environment.blockhash = block.blockhash;
        self.processor.slot = block.slot;
        self.set_sysvars(&block);
    }

    /// Updates the SVM's sysvar cache for the current slot.
    /// For the ER, only `Clock` and `SlotHashes` are relevant and mutable between slots.
    #[inline]
    fn set_sysvars(&self, block: &LatestBlockInner) {
        // SAFETY:
        // This unwrap is safe as no code that could panic holds this specific lock.
        let mut cache = self.processor.writable_sysvar_cache().write().unwrap();
        cache.set_sysvar_for_tests(&block.clock);

        // Avoid a clone by consuming the Arc if we are the only owner, which is
        // guaranteed by the SVM's internal sysvar cache logic.
        let mut hashes = cache
            .get_slot_hashes()
            .ok()
            .and_then(Arc::into_inner)
            .unwrap_or_default();
        hashes.add(block.slot, block.blockhash);
        cache.set_sysvar_for_tests(&hashes);
    }
}

/// A dummy, low-overhead implementation of the `ForkGraph` trait.
#[derive(Default)]
pub(super) struct SimpleForkGraph;

impl ForkGraph for SimpleForkGraph {
    fn relationship(&self, _: u64, _: u64) -> BlockRelation {
        BlockRelation::Unrelated
    }
}

// SAFETY:
// The trait is not automatically derived due to a type within the SVM (`dyn SVMRentCollector`).
// This is considered safe because the concrete `RentCollector` type used at runtime is `Send`.
unsafe impl Send for TransactionExecutor {}

mod callback;
mod processing;
