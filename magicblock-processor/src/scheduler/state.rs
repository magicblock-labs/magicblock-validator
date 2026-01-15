use std::sync::{Arc, OnceLock, RwLock};

use magicblock_accounts_db::AccountsDb;
use magicblock_core::link::{
    accounts::AccountUpdateTx,
    transactions::{
        ScheduledTasksTx, TransactionStatusTx, TransactionToProcessRx,
    },
};
use magicblock_ledger::Ledger;
use serde::Serialize;
use solana_account::AccountSharedData;
use solana_bpf_loader_program::syscalls::{
    create_program_runtime_environment_v1,
    create_program_runtime_environment_v2,
};
use solana_program::{
    clock::DEFAULT_SLOTS_PER_EPOCH, epoch_schedule::EpochSchedule,
    slot_hashes::SlotHashes, sysvar,
};
use solana_program_runtime::{
    loaded_programs::ProgramCache, solana_sbpf::program::BuiltinProgram,
};
use solana_pubkey::Pubkey;
use solana_svm::transaction_processor::TransactionProcessingEnvironment;
use tokio_util::sync::CancellationToken;

use crate::executor::SimpleForkGraph;

/// Holds global state and communication channels for the transaction scheduler.
pub struct TransactionSchedulerState {
    // Global State Handles
    pub accountsdb: Arc<AccountsDb>,
    pub ledger: Arc<Ledger>,
    pub environment: TransactionProcessingEnvironment<'static>,

    // Communication Channels
    pub txn_to_process_rx: TransactionToProcessRx,
    pub account_update_tx: AccountUpdateTx,
    pub transaction_status_tx: TransactionStatusTx,
    pub tasks_tx: ScheduledTasksTx,

    // Configuration
    pub is_auto_airdrop_lamports_enabled: bool,
    pub shutdown: CancellationToken,
}

impl TransactionSchedulerState {
    /// Initializes the shared BPF program cache.
    pub(crate) fn prepare_programs_cache(
        &self,
    ) -> Arc<RwLock<ProgramCache<SimpleForkGraph>>> {
        static FORK_GRAPH: OnceLock<Arc<RwLock<SimpleForkGraph>>> =
            OnceLock::new();

        // Singleton fork graph (validator does not support forks).
        let forkgraph = Arc::downgrade(
            FORK_GRAPH.get_or_init(|| Arc::new(RwLock::new(SimpleForkGraph))),
        );

        let runtime_v1 = create_program_runtime_environment_v1(
            &self.environment.feature_set,
            &Default::default(),
            false,
            false,
        )
        .map(Into::into)
        .unwrap_or_else(|_| {
            Arc::new(BuiltinProgram::new_loader(Default::default()))
        });

        let runtime_v2 =
            create_program_runtime_environment_v2(&Default::default(), false);

        let mut cache = ProgramCache::new(self.accountsdb.slot(), 0);
        cache.set_fork_graph(forkgraph);
        cache.environments.program_runtime_v1 = runtime_v1;
        cache.environments.program_runtime_v2 = runtime_v2.into();

        Arc::new(RwLock::new(cache))
    }

    /// Ensures essential sysvar accounts (Clock, Rent, etc.) exist in `AccountsDb`.
    pub(crate) fn prepare_sysvars(&self) {
        let block = self.ledger.latest_block().load();

        // 1. Mutable Sysvars (updated per block)
        self.ensure_sysvar(&sysvar::clock::ID, &block.clock);

        let slot_hashes = SlotHashes::new(&[(block.slot, block.blockhash)]);
        self.ensure_sysvar(&sysvar::slot_hashes::ID, &slot_hashes);

        // 2. Immutable/Static Sysvars
        let epoch_schedule = EpochSchedule::new(DEFAULT_SLOTS_PER_EPOCH);
        self.ensure_sysvar(&sysvar::epoch_schedule::ID, &epoch_schedule);

        if let Some(rent) = self
            .environment
            .rent_collector
            .as_ref()
            .map(|rc| rc.get_rent())
        {
            self.ensure_sysvar(&sysvar::rent::ID, &rent);
        }
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
