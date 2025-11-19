use std::sync::{Arc, OnceLock, RwLock};

use magicblock_accounts_db::AccountsDb;
use magicblock_core::link::{
    accounts::AccountUpdateTx,
    transactions::{
        ScheduledTasksTx, TransactionStatusTx, TransactionToProcessRx,
    },
};
use magicblock_ledger::Ledger;
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
use solana_svm::transaction_processor::TransactionProcessingEnvironment;

use crate::executor::SimpleForkGraph;

/// A container for the shared state and communication
/// channels required by the `TransactionScheduler`.
///
/// This struct acts as a container for the entire transaction processing pipeline,
/// holding all the necessary handles to global state and communication endpoints.
pub struct TransactionSchedulerState {
    /// A handle to the globally shared accounts database.
    pub accountsdb: Arc<AccountsDb>,
    /// A handle to the globally shared ledger of blocks and transactions.
    pub ledger: Arc<Ledger>,
    /// The shared, reusable Solana SVM processing environment.
    pub environment: TransactionProcessingEnvironment<'static>,
    /// The receiving end of the queue where all new transactions are submitted for processing.
    pub txn_to_process_rx: TransactionToProcessRx,
    /// The channel for sending account state updates to downstream consumers.
    pub account_update_tx: AccountUpdateTx,
    /// The channel for sending final transaction statuses to downstream consumers.
    pub transaction_status_tx: TransactionStatusTx,
    /// A channel to send scheduled (crank) tasks created by transactions.
    pub tasks_tx: ScheduledTasksTx,
    /// True when auto airdrop for fee payers is enabled.
    pub is_auto_airdrop_lamports_enabled: bool,
}

impl TransactionSchedulerState {
    /// Initializes and configures the globally shared BPF program cache.
    ///
    /// This cache is shared among all `TransactionExecutor` workers to avoid
    /// redundant program compilations and loads, improving performance.
    pub(crate) fn prepare_programs_cache(
        &self,
    ) -> Arc<RwLock<ProgramCache<SimpleForkGraph>>> {
        static FORK_GRAPH: OnceLock<Arc<RwLock<SimpleForkGraph>>> =
            OnceLock::new();

        // Use a static singleton for the fork graph as this validator does not handle forks.
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
        .unwrap_or(Arc::new(BuiltinProgram::new_loader(
            solana_program_runtime::solana_sbpf::vm::Config::default(),
        )));
        let runtime_v2 =
            create_program_runtime_environment_v2(&Default::default(), false);
        let mut cache = ProgramCache::new(self.accountsdb.slot(), 0);
        cache.set_fork_graph(forkgraph);

        cache.environments.program_runtime_v1 = runtime_v1;
        cache.environments.program_runtime_v2 = runtime_v2.into();
        Arc::new(RwLock::new(cache))
    }

    /// Ensures that all necessary sysvar accounts are present in the `AccountsDb` at startup.
    ///
    /// This is a one-time setup step to populate the database with essential on-chain state
    /// (like `Clock`, `Rent`, etc.) that programs may need to access during execution.
    pub(crate) fn prepare_sysvars(&self) {
        let owner = &sysvar::ID;
        let accountsdb = &self.accountsdb;

        // Initialize mutable sysvars based on the latest block.
        let block = self.ledger.latest_block().load();
        if !accountsdb.contains_account(&sysvar::clock::ID) {
            let clock = AccountSharedData::new_data(1, &block.clock, owner);
            if let Ok(acc) = clock {
                accountsdb.insert_account(&sysvar::clock::ID, &acc);
            }
        }
        if !accountsdb.contains_account(&sysvar::slot_hashes::ID) {
            let sh = SlotHashes::new(&[(block.slot, block.blockhash)]);
            if let Ok(acc) = AccountSharedData::new_data(1, &sh, owner) {
                accountsdb.insert_account(&sysvar::slot_hashes::ID, &acc);
            }
        }

        // Initialize sysvars that are immutable for the lifetime of the validator.
        if !accountsdb.contains_account(&sysvar::epoch_schedule::ID) {
            let es = EpochSchedule::new(DEFAULT_SLOTS_PER_EPOCH);
            if let Ok(acc) = AccountSharedData::new_data(1, &es, owner) {
                accountsdb.insert_account(&sysvar::epoch_schedule::ID, &acc);
            }
        }
        if !accountsdb.contains_account(&sysvar::rent::ID) {
            let account = self
                .environment
                .rent_collector
                .as_ref()
                .map(|rc| rc.get_rent())
                .and_then(|rent| {
                    AccountSharedData::new_data(1, rent, owner).ok()
                });
            if let Some(acc) = account {
                accountsdb.insert_account(&sysvar::rent::ID, &acc);
            }
        }
    }
}
