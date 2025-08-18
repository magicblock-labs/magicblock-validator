use std::sync::{Arc, OnceLock, RwLock};

use magicblock_accounts_db::AccountsDb;
use magicblock_core::link::{
    accounts::AccountUpdateTx,
    transactions::{TransactionStatusTx, TransactionToProcessRx},
};
use magicblock_ledger::{LatestBlock, Ledger};
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

pub struct TransactionSchedulerState {
    pub accountsdb: Arc<AccountsDb>,
    pub ledger: Arc<Ledger>,
    pub latest_block: LatestBlock,
    pub environment: TransactionProcessingEnvironment<'static>,
    pub txn_to_process_rx: TransactionToProcessRx,
    pub account_update_tx: AccountUpdateTx,
    pub transaction_status_tx: TransactionStatusTx,
}

impl TransactionSchedulerState {
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

    pub(crate) fn prepare_sysvars(&self) {
        let owner = &sysvar::ID;
        let accountsdb = &self.accountsdb;

        if !accountsdb.contains_account(&sysvar::clock::ID) {
            let clock = &self.latest_block.load().clock;
            if let Ok(acc) = AccountSharedData::new_data(1, clock, owner) {
                accountsdb.insert_account(&sysvar::clock::ID, &acc);
            }
        }
        if !accountsdb.contains_account(&sysvar::slot_hashes::ID) {
            let block = &self.latest_block.load();
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
