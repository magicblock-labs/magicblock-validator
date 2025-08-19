use std::{collections::btree_map::Keys, sync::Arc};

use magicblock_accounts_db::AccountsDb;
use magicblock_core::{
    link::{
        link,
        transactions::{SanitizeableTransaction, TransactionSchedulerHandle},
        RpcChannelEndpoints,
    },
    magic_program::Pubkey,
};
use magicblock_ledger::Ledger;
use magicblock_processor::{
    build_svm_env,
    scheduler::{state::TransactionSchedulerState, TransactionScheduler},
};
use tempfile::TempDir;

pub struct ExecutionTestEnv {
    pub accountsdb: Arc<AccountsDb>,
    pub ledger: Arc<Ledger>,
    pub transaction_scheduler: TransactionSchedulerHandle,
    pub dir: TempDir,
    pub rpc_channels: RpcChannelEndpoints,
}

impl ExecutionTestEnv {
    pub fn new() -> Self {
        let dir =
            tempfile::tempdir().expect("creating temp dir for validator state");
        let accountsdb = Arc::new(
            AccountsDb::open(dir.path()).expect("opening test accountsdb"),
        );
        let ledger =
            Arc::new(Ledger::open(dir.path()).expect("opening test ledger"));
        let (rpc_channels, validator_channels) = link();
        let latest_block = ledger.latest_block().clone();
        let environment =
            build_svm_env(&accountsdb, latest_block.load().blockhash, 0);
        let scheduler_state = TransactionSchedulerState {
            accountsdb: accountsdb.clone(),
            ledger: ledger.clone(),
            account_update_tx: validator_channels.account_update,
            transaction_status_tx: validator_channels.transaction_status,
            latest_block,
            txn_to_process_rx: validator_channels.transaction_to_process,
            environment,
        };
        TransactionScheduler::new(1, scheduler_state).spawn();
        Self {
            accountsdb,
            ledger,
            transaction_scheduler: rpc_channels.transaction_scheduler.clone(),
            dir,
            rpc_channels,
        }
    }

    pub fn create_account(&self, lamports: u64) -> Keypair {
        let keypair = Keypair::new();
    }

    pub fn fund_account(&self, pubkey: Pubkey, lamports: u64) {}

    pub fn execute_transaction(&self, txn: impl SanitizeableTransaction) {}
}
