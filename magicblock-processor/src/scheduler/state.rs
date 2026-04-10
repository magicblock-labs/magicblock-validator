//! Shared state initialization for the transaction scheduler.
//!
//! Prepares global resources: program cache, sysvars, and communication channels.
//! `TransactionSchedulerState` is constructed externally and passed to
//! `TransactionScheduler::new()` for initialization.

use std::sync::{Arc, OnceLock, RwLock};

use magicblock_accounts_db::{traits::AccountsBank, AccountsDb};
use magicblock_core::link::{
    accounts::AccountUpdateTx,
    transactions::{
        ScheduledTasksTx, TransactionStatusTx, TransactionToProcessRx,
    },
};
use magicblock_ledger::Ledger;
use serde::Serialize;
use solana_account::AccountSharedData;
use solana_feature_set::FeatureSet;
use solana_program::{
    clock::DEFAULT_SLOTS_PER_EPOCH,
    epoch_schedule::EpochSchedule,
    slot_hashes::{SlotHashes, MAX_ENTRIES},
    sysvar,
};
use solana_program_runtime::loaded_programs::ProgramCache;
use solana_pubkey::Pubkey;
use solana_svm::transaction_processor::TransactionProcessingEnvironment;
use tokio::sync::mpsc::Receiver;
use tokio_util::sync::CancellationToken;

use crate::executor::SimpleForkGraph;

/// Container for global state and communication channels.
///
/// Holds all shared resources needed to bootstrap the scheduler.
/// Created externally and consumed by `TransactionScheduler::new()`.
pub struct TransactionSchedulerState {
    // === Global State Handles ===
    pub accountsdb: Arc<AccountsDb>,
    pub ledger: Arc<Ledger>,
    pub environment: TransactionProcessingEnvironment,
    pub feature_set: FeatureSet,

    // === Communication Channels ===
    pub txn_to_process_rx: TransactionToProcessRx,
    pub account_update_tx: AccountUpdateTx,
    pub transaction_status_tx: TransactionStatusTx,
    pub tasks_tx: ScheduledTasksTx,

    pub shutdown: CancellationToken,
    /// Receives mode transition commands (Primary or Replica) at runtime.
    pub mode_rx: Receiver<SchedulerMode>,
}

impl TransactionSchedulerState {
    /// Initializes the shared BPF program cache.
    pub(crate) fn prepare_programs_cache(
        &self,
    ) -> Arc<RwLock<ProgramCache<SimpleForkGraph>>> {
        static FORK_GRAPH: OnceLock<Arc<RwLock<SimpleForkGraph>>> =
            OnceLock::new();

        // Singleton fork graph: validator doesn't support forks
        let forkgraph = Arc::downgrade(
            FORK_GRAPH.get_or_init(|| Arc::new(RwLock::new(SimpleForkGraph))),
        );

        let mut cache = ProgramCache::new(self.accountsdb.slot());
        cache.set_fork_graph(forkgraph);

        Arc::new(RwLock::new(cache))
    }

    /// Ensures essential sysvar accounts exist in `AccountsDb`.
    pub(crate) fn prepare_sysvars(&self) {
        let block = self.ledger.latest_block().load();

        // Mutable sysvars (updated on each slot transition)
        self.ensure_sysvar(&sysvar::clock::ID, &block.clock);

        let slot_hashes =
            SlotHashes::new(&[(block.slot, block.blockhash); MAX_ENTRIES]);
        // Remove first to avoid "account already exists" errors
        self.accountsdb.remove_account(&sysvar::slot_hashes::ID);
        self.ensure_sysvar(&sysvar::slot_hashes::ID, &slot_hashes);

        // Immutable/Static sysvars (initialized once)
        let epoch_schedule = EpochSchedule::new(DEFAULT_SLOTS_PER_EPOCH);
        self.ensure_sysvar(&sysvar::epoch_schedule::ID, &epoch_schedule);

        self.ensure_sysvar(&sysvar::rent::ID, &self.environment.rent);
    }

    /// Helper to serialize and insert a sysvar if it doesn't exist.
    fn ensure_sysvar<T: Serialize>(&self, id: &Pubkey, data: &T) {
        if self.accountsdb.contains_account(id) {
            return;
        }
        if let Ok(account) = AccountSharedData::new_data(1, data, &sysvar::ID) {
            let _ = self.accountsdb.insert_account(id, &account);
        }
    }
}

/// Scheduler execution mode command.
///
/// Send via channel to transition the scheduler between modes.
/// See [`CoordinationMode`](super::coordinator::CoordinationMode) for internal state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SchedulerMode {
    /// Accept client transactions with concurrent execution.
    Primary,
    /// Replay transactions with strict ordering.
    Replica,
}
