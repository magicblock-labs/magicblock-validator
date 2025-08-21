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

/// Isolated SVM worker, the only entity responsible for processing transactions
pub(super) struct TransactionExecutor {
    /// SVM worker ID
    id: WorkerId,
    /// Global accounts database
    accountsdb: Arc<AccountsDb>,
    /// Global ledger of blocks/transactions
    ledger: Arc<Ledger>,
    /// Internal solana SVM entrypoint
    processor: TransactionBatchProcessor<SimpleForkGraph>,
    /// Immutable configuration for transaction processing, set at startup
    config: Box<TransactionProcessingConfig<'static>>,
    /// Globaly shared state of the latest block, updated by the ledger
    block: LatestBlock,
    /// Reusable SVM environment for transaction processing
    environment: TransactionProcessingEnvironment<'static>,
    /// A channel from TransactionScheduler, the only source of transactions to process
    rx: TransactionToProcessRx,
    /// A channel to forward transaction execution status to downstream consumers (RPC/Geyser)
    transaction_tx: TransactionStatusTx,
    /// A channel to forward account state updates to downstream consumers (RPC/Geyser)
    accounts_tx: AccountUpdateTx,
    /// A back channel to communicate workder readiness to
    /// process more transactions back to the scheduler
    ready_tx: Sender<WorkerId>,
    /// Synchronization lock to stop all processing during critical operations
    sync: StWLock,
    /// Atomically incremented intra-slot index of transactions
    // TODO(bmuddha): get rid of explicit indexing, once the
    // new ledger is implemented (with implicit indexing based
    // on the position of transaction in the ledger file)
    index: Arc<AtomicUsize>,
}

impl TransactionExecutor {
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
        // override the default program cache of this processor with a global
        // one, which is shared between all of the running processor instances,
        // this is mostly an optimization, so a change in the program cache of
        // one one executor is immediately available to the rest, instead of
        // waiting for them to update their own caches on a new program encounter
        processor.program_cache = programs_cache;
        // NOTE: setting all of the recording settings to true, as we do here, can have
        // a noticeable impact on performance due to all of the extra logging involved
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
            block: state.latest_block.clone(),
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

    /// Register all of the builtin programs with the given transaction executor
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

    /// Spawn the transaction executor in isolated OS thread with a dedicated async runtime
    pub(super) fn spawn(self) {
        // For performance reasons, each transaction executor needs to operate within
        // its own OS thread, but at the same time it needs some concurrency support,
        // which is why we spawn it with a dedicated single threaded tokio runtime
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

    /// Start running the transaction executor, by accepting incoming transaction to process
    async fn run(mut self) {
        // at the start of each slot, we need to acquire the synchronization lock,
        // to ensure that no critical operation like accountsdb snapshotting can
        // take place, while transactions are being executed. The lock is held for
        // the duration of slot, and then it's released at slot boundaries, to allow
        // for any pending critical operation to be run, before re-acquisition.
        let mut _guard = self.sync.read();
        let mut block_updated = self.block.subscribe();

        loop {
            tokio::select! {
                // Transactions to process, the source is the TransactionScheduler
                biased; Some(txn) = self.rx.recv() => {
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
                    let _ = self.ready_tx.send(self.id).await;
                }
                // A new block has been produced, the source is the Ledger itself
                _ = block_updated.recv() => {
                    // explicitly release the lock in a fair manner, allowing
                    // any pending lock acquisition request to succeed
                    RwLockReadGuard::unlock_fair(_guard);
                    // update slot relevant state
                    self.transition_to_new_slot();
                    // and then re-acquire the lock for another slot duration
                    // NOTE: in most cases the re-acquisition is almost instantaneous, the only
                    // case when "hiccup" can occur is when some critical operation which needs to
                    // stop all the activity (like accountsdb snapshotting) needs to take place
                    _guard = self.sync.read();
                }
                // system is shutting down, no more transactions will follow
                else => {
                    break;
                }
            }
        }
        info!("transaction executor {} has terminated", self.id)
    }

    /// Update slot related slot to work with latest produced block
    fn transition_to_new_slot(&mut self) {
        let block = self.block.load();
        // most of the execution environment is immutable, with an exception
        // of the blockhash, which we update here with every new block
        self.environment.blockhash = block.blockhash;
        self.processor.slot = block.slot;
        self.set_sysvars(&block);
    }

    /// Set sysvars, which are relevant in the context of ER, currently those are:
    /// - Clock
    /// - SlotHashes
    ///
    /// everything else, like Rent, EpochSchedule, StakeHistory, etc.
    /// either is immutable or doesn't pertain to the ER operation
    #[inline]
    fn set_sysvars(&self, block: &LatestBlockInner) {
        // SAFETY:
        // unwrap here is safe, as we don't have any code which might panic while holding
        // this particular lock, but if we do introduce such a code in the future, then
        // panic propagation is probably what is desired
        let mut cache = self.processor.writable_sysvar_cache().write().unwrap();

        cache.set_sysvar_for_tests(&block.clock);

        // and_then(Arc::into_inner) will always succeed as get_slot_hashes
        // always returns a unique Arc reference, which allows us to avoid
        // extra clone in order to construct a mutable intance of SlotHashes
        let mut hashes = cache
            .get_slot_hashes()
            .ok()
            .and_then(Arc::into_inner)
            .unwrap_or_default();
        hashes.add(block.slot, block.blockhash);
        cache.set_sysvar_for_tests(&hashes);
    }
}

/// Dummy, low overhead, ForkGraph implementation
#[derive(Default)]
pub(super) struct SimpleForkGraph;

impl ForkGraph for SimpleForkGraph {
    /// we never have state forks, hence no relevant handling
    /// logic, so we don't really care about those relations
    fn relationship(&self, _: u64, _: u64) -> BlockRelation {
        BlockRelation::Unrelated
    }
}

/// SAFETY:
/// The complaint is about SVMRentCollector trait object which doesn't have
/// Send bound, but we use ordinary RentCollector, which is Send + 'static
unsafe impl Send for TransactionExecutor {}

mod callback;
mod processing;
