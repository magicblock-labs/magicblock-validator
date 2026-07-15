use std::{
    cmp::Ordering,
    collections::BTreeMap,
    ops::Deref,
    sync::{Arc, RwLock},
};

use magicblock_accounts_db::AccountsDb;
use magicblock_core::{
    link::{
        accounts::AccountUpdateTx,
        blocks::LatestBlockInner,
        transactions::{
            ProcessableTransaction, ScheduledTasksTx,
            TransactionProcessingMode, TransactionStatusTx,
        },
    },
    Slot,
};
use magicblock_ledger::{LatestBlock, Ledger};
use magicblock_program::sysvar::HighPrecisionClock;
use solana_feature_set::FeatureSet;
use solana_program::slot_hashes::SlotHashes;
use solana_program_runtime::loaded_programs::{
    BlockRelation, ForkGraph, ProgramCache, ProgramCacheEntry,
};
use solana_svm::transaction_processor::{
    ExecutionRecordingConfig, TransactionBatchProcessor,
    TransactionProcessingConfig, TransactionProcessingEnvironment,
};
use solana_transaction::sanitized::SanitizedTransaction;
use tokio::{
    runtime::Builder,
    sync::{
        broadcast,
        mpsc::{Receiver, Sender},
        oneshot, Semaphore,
    },
};
use tracing::{info, instrument, warn};

use crate::{
    builtins::BUILTINS,
    scheduler::{locks::ExecutorId, state::TransactionSchedulerState},
};

const BLOCK_HISTORY_SIZE: usize = 32;

pub(crate) struct IndexedTransaction {
    pub(crate) slot: Slot,
    pub(crate) index: u32,
    pub(crate) txn: ProcessableTransaction,
}

#[allow(clippy::large_enum_variant)]
pub(crate) enum ExecutorCommand {
    Transaction(IndexedTransaction),
    Block {
        block: LatestBlockInner,
        ack: oneshot::Sender<()>,
    },
}

impl Deref for IndexedTransaction {
    type Target = SanitizedTransaction;
    fn deref(&self) -> &Self::Target {
        &self.txn.transaction
    }
}

/// A dedicated, single-threaded worker responsible for processing transactions.
pub(super) struct TransactionExecutor {
    id: ExecutorId,

    // State Handles
    accountsdb: Arc<AccountsDb>,
    ledger: Arc<Ledger>,
    block: LatestBlock,
    block_history: BTreeMap<Slot, LatestBlockInner>,
    execution_permits: Arc<Semaphore>,

    // SVM Components
    processor: TransactionBatchProcessor<SimpleForkGraph>,
    config: Box<TransactionProcessingConfig<'static>>,
    environment: TransactionProcessingEnvironment,
    feature_set: FeatureSet,

    // Channels
    rx: Receiver<ExecutorCommand>,
    transaction_tx: TransactionStatusTx,
    accounts_tx: AccountUpdateTx,
    tasks_tx: ScheduledTasksTx,
    ready_tx: Sender<ExecutorId>,
}

impl TransactionExecutor {
    pub(super) fn new(
        id: ExecutorId,
        state: &TransactionSchedulerState,
        rx: Receiver<ExecutorCommand>,
        ready_tx: Sender<ExecutorId>,
        execution_permits: Arc<Semaphore>,
        programs_cache: Arc<RwLock<ProgramCache<SimpleForkGraph>>>,
    ) -> Self {
        let slot = state.accountsdb.slot();
        let mut processor =
            TransactionBatchProcessor::new_uninitialized(slot, 0);

        // Use global program cache to share compilation results across executors
        processor.global_program_cache = programs_cache;
        processor.environments = state
            .environment
            .program_runtime_environments_for_execution
            .clone();

        // Enable recording for accurate fee/unit usage tracking
        let config = Box::new(TransactionProcessingConfig {
            recording_config: ExecutionRecordingConfig::new_single_setting(
                true,
            ),
            limit_to_load_programs: true,
            ..Default::default()
        });

        let block = state.ledger.latest_block();
        let initial_block = LatestBlockInner::clone(&*block.load());

        let mut block_history = BTreeMap::new();
        block_history.insert(initial_block.slot, initial_block.clone());

        let this = Self {
            id,
            processor,
            accountsdb: state.accountsdb.clone(),
            ledger: state.ledger.clone(),
            config,
            block: block.clone(),
            environment: copy_env(&state.environment),
            feature_set: state.feature_set.clone(),
            execution_permits,
            rx,
            ready_tx,
            accounts_tx: state.account_update_tx.clone(),
            transaction_tx: state.transaction_status_tx.clone(),
            tasks_tx: state.tasks_tx.clone(),
            block_history,
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
            self.processor.add_builtin(builtin.program_id, entry);
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
        let mut block_updated = self.block.subscribe();

        loop {
            tokio::select! {
                biased;
                result = block_updated.recv() => {
                    match result {
                        Ok(latest) => self.register_new_block(latest),
                        Err(broadcast::error::RecvError::Lagged(_)) => {
                            self.register_new_block(self.block.load().as_ref().clone());
                        }
                        Err(broadcast::error::RecvError::Closed) => break,
                    }
                }
                command = self.rx.recv() => {
                    let Some(command) = command else { break };
                    match command {
                        ExecutorCommand::Transaction(transaction) => {
                            if transaction.slot != self.processor.slot {
                                self.transition_to_slot(transaction.slot);
                            }
                            let _permit = self.execution_permits.acquire().await;
                            match transaction.txn.mode {
                                TransactionProcessingMode::Execution(_) => {
                                    self.execute(transaction, None);
                                }
                                TransactionProcessingMode::Simulation(tx) => {
                                    self.simulate([transaction.txn.transaction], tx);
                                }
                                TransactionProcessingMode::Replay(ctx) => {
                                    self.execute(transaction, Some(ctx.persist));
                                }
                            }
                            let _ = self.ready_tx.try_send(self.id);
                        }
                        ExecutorCommand::Block { block, ack } => {
                            self.apply_block(block);
                            let _ = ack.send(());
                        }
                    }
                }
                else => break,
            }
        }
        info!("Executor terminated");
    }

    fn register_new_block(&mut self, block: LatestBlockInner) {
        while self.block_history.len() >= BLOCK_HISTORY_SIZE {
            self.block_history.pop_first();
        }
        self.block_history.insert(block.slot, block);
    }

    fn apply_block(&mut self, block: LatestBlockInner) {
        let slot = block.clock.slot;
        self.register_new_block(block.clone());
        self.environment.blockhash = block.blockhash;
        self.processor.slot = slot;
        self.set_sysvars(&block);
    }

    fn transition_to_slot(&mut self, slot: Slot) {
        // transactions execute in the latest finalized block + 1
        let prev_slot = slot.saturating_sub(1);
        let Some(block) = self.block_history.get(&prev_slot) else {
            // should never happen in practice
            warn!(slot, "tried to transition to slot which wasn't registered");
            return;
        };
        self.environment.blockhash = block.blockhash;
        self.processor.slot = slot;
        self.set_sysvars(block);
    }

    /// Updates cache and persists slot hashes.
    fn set_sysvars(&self, block: &LatestBlockInner) {
        let mut cache = self.processor.writable_sysvar_cache().write().unwrap();
        cache.set_sysvar_for_tests(&block.clock);
        cache.set_sysvar_for_tests(&HighPrecisionClock {
            unix_timestamp_millis: block.timestamp_millis,
        });

        if let Ok(hashes) = cache.get_slot_hashes() {
            let mut hashes = SlotHashes::new(hashes.slot_hashes());
            hashes.add(block.slot, block.blockhash);
            cache.set_sysvar_for_tests(&hashes);
        }
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

fn copy_env(
    env: &TransactionProcessingEnvironment,
) -> TransactionProcessingEnvironment {
    TransactionProcessingEnvironment {
        blockhash: env.blockhash,
        blockhash_lamports_per_signature: env.blockhash_lamports_per_signature,
        epoch_total_stake: env.epoch_total_stake,
        feature_set: env.feature_set,
        program_runtime_environments_for_execution: env
            .program_runtime_environments_for_execution
            .clone(),
        program_runtime_environments_for_deployment: env
            .program_runtime_environments_for_deployment
            .clone(),
        rent: env.rent.clone(),
    }
}

mod callback;
mod processing;
