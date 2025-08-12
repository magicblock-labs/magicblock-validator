use std::sync::{atomic::AtomicUsize, Arc, OnceLock, RwLock};

use magicblock_accounts_db::{AccountsDb, StWLock};
use magicblock_core::link::{
    accounts::AccountUpdateTx,
    transactions::{TransactionProcessingMode, TxnStatusTx, TxnToProcessRx},
};
use magicblock_ledger::{LatestBlock, Ledger};
use parking_lot::RwLockReadGuard;
use solana_bpf_loader_program::syscalls::{
    create_program_runtime_environment_v1,
    create_program_runtime_environment_v2,
};
use solana_program::sysvar;
use solana_program_runtime::loaded_programs::{
    BlockRelation, ForkGraph, ProgramCacheEntry,
};
use solana_svm::transaction_processor::{
    ExecutionRecordingConfig, TransactionBatchProcessor,
    TransactionProcessingConfig, TransactionProcessingEnvironment,
};
use tokio::{runtime::Builder, sync::mpsc::Sender};

use crate::{
    builtins::BUILTINS, scheduler::TransactionSchedulerState, WorkerId,
};

static FORK_GRAPH: OnceLock<Arc<RwLock<SimpleForkGraph>>> = OnceLock::new();

pub(super) struct TransactionExecutor {
    id: WorkerId,
    accountsdb: Arc<AccountsDb>,
    ledger: Arc<Ledger>,
    processor: TransactionBatchProcessor<SimpleForkGraph>,
    config: Box<TransactionProcessingConfig<'static>>,
    block: LatestBlock,
    environment: TransactionProcessingEnvironment<'static>,
    rx: TxnToProcessRx,
    transaction_tx: TxnStatusTx,
    accounts_tx: AccountUpdateTx,
    ready_tx: Sender<WorkerId>,
    sync: StWLock,
    slot: u64,
    index: Arc<AtomicUsize>,
}

impl TransactionExecutor {
    pub(super) fn new(
        id: WorkerId,
        state: &TransactionSchedulerState,
        rx: TxnToProcessRx,
        ready_tx: Sender<WorkerId>,
        index: Arc<AtomicUsize>,
    ) -> Self {
        let slot = state.accountsdb.slot();
        let forkgraph = Arc::downgrade(
            FORK_GRAPH.get_or_init(|| Arc::new(RwLock::new(SimpleForkGraph))),
        );

        let runtime_v1 = create_program_runtime_environment_v1(
            &state.environment.feature_set,
            &Default::default(),
            false,
            false,
        );
        let runtime_v2 =
            create_program_runtime_environment_v2(&Default::default(), false);
        let processor = TransactionBatchProcessor::new(
            slot,
            Default::default(),
            forkgraph,
            runtime_v1.map(Into::into).ok(),
            Some(runtime_v2.into()),
        );
        let recording_config =
            ExecutionRecordingConfig::new_single_setting(true);
        let config = Box::new(TransactionProcessingConfig {
            recording_config,
            ..Default::default()
        });
        let this = Self {
            id,
            slot: state.block.load().slot,
            sync: state.accountsdb.synchronizer(),
            processor,
            accountsdb: state.accountsdb.clone(),
            ledger: state.ledger.clone(),
            config,
            block: state.block.clone(),
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

    pub(super) fn spawn(self) {
        let task = move || {
            let runtime = Builder::new_current_thread()
                .thread_name("transaction executor")
                .build()
                .expect(
                    "building single threaded tokio runtime should succeed",
                );
            runtime.block_on(tokio::task::unconstrained(self.run()));
        };
        std::thread::spawn(task);
    }

    async fn run(mut self) {
        let mut _guard = self.sync.read();
        loop {
            tokio::select! {
                biased; Some(txn) = self.rx.recv() => {
                    match txn.mode {
                        TransactionProcessingMode::Execution(tx) => {
                            self.execute([txn.transaction], tx);
                        }
                        TransactionProcessingMode::Simulation(tx) => {
                            self.simulate([txn.transaction], tx);
                        }
                    }
                    let _ = self.ready_tx.send(self.id).await;
                }
                _ = self.block.changed() => {
                    let block = self.block.load();
                    self.environment.blockhash = block.blockhash;
                    self.processor.set_slot(block.slot);
                    self.slot = block.slot;
                    self.processor.writable_sysvar_cache()
                        .write().unwrap().set_sysvar_for_tests(&block.clock);
                    self.processor.program_cache.write()
                        .unwrap().latest_root_slot = block.slot;
                    RwLockReadGuard::unlock_fair(_guard);
                    _guard = self.sync.read();
                }
                else => {
                    break;
                }
            }
        }
    }
}

/// Dummy, low overhead, ForkGraph implementation
#[derive(Default)]
pub(super) struct SimpleForkGraph;

impl ForkGraph for SimpleForkGraph {
    /// we never have forks or relevant logic, so we
    /// don't really care about those relations
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
