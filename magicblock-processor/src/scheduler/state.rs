use std::sync::{Arc, OnceLock, RwLock};

use magicblock_accounts_db::AccountsDb;
use magicblock_core::link::{
    accounts::AccountUpdateTx,
    transactions::{TransactionStatusTx, TransactionToProcessRx},
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

/// A bag of various states/channels, necessary to operate the transaction scheduler
pub struct TransactionSchedulerState {
    /// Globally shared accounts database
    pub accountsdb: Arc<AccountsDb>,
    /// Globally shared blocks/transactions ledger
    pub ledger: Arc<Ledger>,
    /// Reusable SVM environment for transaction processing
    pub environment: TransactionProcessingEnvironment<'static>,
    /// A consumer endpoint for all of the transactions originating throughout the validator
    pub txn_to_process_rx: TransactionToProcessRx,
    /// A channel to forward account state updates to downstream consumers (RPC/Geyser)
    pub account_update_tx: AccountUpdateTx,
    /// A channel to forward transaction execution status to downstream consumers (RPC/Geyser)
    pub transaction_status_tx: TransactionStatusTx,
}

impl TransactionSchedulerState {
    /// Setup the shared program cache with runtime environments
    pub(crate) fn prepare_programs_cache(
        &self,
    ) -> Arc<RwLock<ProgramCache<SimpleForkGraph>>> {
        static FORK_GRAPH: OnceLock<Arc<RwLock<SimpleForkGraph>>> =
            OnceLock::new();

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

    /// Make sure all the sysvars that are necessary for ER operation are present in the accountsdb
    pub(crate) fn prepare_sysvars(&self) {
        let owner = &sysvar::ID;
        let accountsdb = &self.accountsdb;

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
                accountsdb.insert_account(&sysvar::epoch_schedule::ID, &acc);
            }
        }

        // The following sysvars are immutable for the run time of the validator
        if !accountsdb.contains_account(&sysvar::epoch_schedule::ID) {
            // since we don't have epochs, any value will do
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
