use std::{
    cmp::Ordering,
    convert::identity,
    sync::{Arc, RwLock},
};

use magicblock_accounts_db::{AccountsDb, GlobalWriteLock};
use magicblock_core::link::{
    accounts::AccountUpdateTx,
    transactions::{
        ScheduledTasksTx, TransactionProcessingMode, TransactionStatusTx,
        TransactionToProcessRx,
    },
};
use magicblock_ledger::{LatestBlock, LatestBlockInner, Ledger};
use parking_lot::RwLockReadGuard;
use serde::Serialize;
use solana_program_runtime::loaded_programs::{
    BlockRelation, ForkGraph, ProgramCache, ProgramCacheEntry,
};
use solana_pubkey::Pubkey;
use solana_sdk_ids::sysvar::{clock, slot_hashes};
use solana_svm::transaction_processor::{
    ExecutionRecordingConfig, TransactionBatchProcessor,
    TransactionProcessingConfig, TransactionProcessingEnvironment,
};
use tokio::{runtime::Builder, sync::mpsc::Sender};
use tracing::{info, instrument, warn};

use crate::{
    builtins::BUILTINS,
    scheduler::{locks::ExecutorId, state::TransactionSchedulerState},
};

/// A dedicated, single-threaded worker responsible for processing transactions.
pub(super) struct TransactionExecutor {
    id: ExecutorId,

    // State Handles
    accountsdb: Arc<AccountsDb>,
    ledger: Arc<Ledger>,
    block: LatestBlock,
    sync: GlobalWriteLock,

    // SVM Components
    processor: TransactionBatchProcessor<SimpleForkGraph>,
    config: Box<TransactionProcessingConfig<'static>>,
    environment: TransactionProcessingEnvironment<'static>,

    // Channels
    rx: TransactionToProcessRx,
    transaction_tx: TransactionStatusTx,
    accounts_tx: AccountUpdateTx,
    tasks_tx: ScheduledTasksTx,
    ready_tx: Sender<ExecutorId>,

    // Config
    is_auto_airdrop_lamports_enabled: bool,
}

impl TransactionExecutor {
    pub(super) fn new(
        id: ExecutorId,
        state: &TransactionSchedulerState,
        rx: TransactionToProcessRx,
        ready_tx: Sender<ExecutorId>,
        programs_cache: Arc<RwLock<ProgramCache<SimpleForkGraph>>>,
    ) -> Self {
        let slot = state.accountsdb.slot();
        let mut processor = TransactionBatchProcessor::new_uninitialized(
            slot,
            Default::default(),
        );

        // Use global program cache to share compilation results across executors
        processor.program_cache = programs_cache;

        // Enable recording for accurate fee/unit usage tracking
        let config = Box::new(TransactionProcessingConfig {
            recording_config: ExecutionRecordingConfig::new_single_setting(
                true,
            ),
            limit_to_load_programs: true,
            ..Default::default()
        });

        let this = Self {
            id,
            sync: state.accountsdb.write_lock(),
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
            tasks_tx: state.tasks_tx.clone(),
            is_auto_airdrop_lamports_enabled: state
                .is_auto_airdrop_lamports_enabled,
        };

        this.processor.fill_missing_sysvar_cache_entries(&this);
        this
    }

    pub(super) fn populate_builtins(&self) {
        for builtin in BUILTINS {
            let entry = ProgramCacheEntry::new_builtin(
                0,
                builtin.name.len(),
                builtin.entrypoint,
            );
            self.processor.add_builtin(
                self,
                builtin.program_id,
                builtin.name,
                entry,
            );
        }
    }

    pub(super) fn spawn(self) {
        std::thread::spawn(move || {
            let runtime = Builder::new_current_thread()
                .thread_name(format!("txn-executor-{}", self.id))
                .build()
                .expect("Failed to build executor runtime");

            runtime.block_on(tokio::task::unconstrained(self.run()));
        });
    }

    #[allow(clippy::await_holding_lock)]
    #[instrument(skip(self), fields(executor_id = self.id))]
    async fn run(mut self) {
        let mut guard = self.sync.read();
        let mut block_updated = self.block.subscribe();

        loop {
            tokio::select! {
                biased;
                txn = self.rx.recv() => {
                    let Some(txn) = txn else { break };
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
                    let _ = self.ready_tx.try_send(self.id);
                }
                _ = block_updated.recv() => {
                    // Unlock to allow global ops (snapshots), then update slot
                    RwLockReadGuard::unlock_fair(guard);
                    self.transition_to_new_slot();
                    guard = self.sync.read();
                }
                else => break,
            }
        }
        info!("Executor terminated");
    }

    fn transition_to_new_slot(&mut self) {
        let block = self.block.load();
        self.environment.blockhash = block.blockhash;
        self.processor.slot = block.slot;
        self.set_sysvars(&block);
    }

    /// Updates cache and persists slot hashes.
    fn set_sysvars(&self, block: &LatestBlockInner) {
        let mut cache = self.processor.writable_sysvar_cache().write().unwrap();
        cache.set_sysvar_for_tests(&block.clock);

        let mut hashes = cache
            .get_slot_hashes()
            .ok()
            .and_then(Arc::into_inner)
            .unwrap_or_default();

        hashes.add(block.slot, block.blockhash);
        cache.set_sysvar_for_tests(&hashes);
        self.persist_sysvar(slot_hashes::ID, &hashes);
        self.persist_sysvar(clock::ID, &block.clock);
    }

    /// Serialize sysvar account to AccountsDB
    fn persist_sysvar<D: Serialize>(&self, id: Pubkey, data: &D) {
        let Ok(reader) = self.accountsdb.reader() else {
            return;
        };
        let Some(mut account) = reader.read(&id, identity) else {
            return;
        };
        if let Err(e) = account.serialize_data(data) {
            warn!(?e, "Failed to serialize sysvar: {}");
            return;
        }
        let _ = self.accountsdb.insert_account(&slot_hashes::ID, &account);
    }
}

/// A dummy, low-overhead ForkGraph for a linear (forkless) chain.
#[derive(Default)]
pub(super) struct SimpleForkGraph;

impl ForkGraph for SimpleForkGraph {
    fn relationship(&self, a: u64, b: u64) -> BlockRelation {
        match a.cmp(&b) {
            Ordering::Less => BlockRelation::Ancestor,
            Ordering::Greater => BlockRelation::Descendant,
            Ordering::Equal => BlockRelation::Equal,
        }
    }
}

// SAFETY: Required for SVM internals (`dyn SVMRentCollector` interactions).
// Concrete types used here are Send.
unsafe impl Send for TransactionExecutor {}

mod callback;
mod processing;
