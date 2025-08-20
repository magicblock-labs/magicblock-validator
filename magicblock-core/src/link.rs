use accounts::{
    AccountUpdateRx, AccountUpdateTx, EnsureAccountsRx, EnsureAccountsTx,
};
use blocks::{BlockUpdateRx, BlockUpdateTx};
use tokio::sync::mpsc;
use transactions::{
    TransactionSchedulerHandle, TransactionStatusRx, TransactionStatusTx,
    TransactionToProcessRx,
};

pub mod accounts;
pub mod blocks;
pub mod transactions;

pub type Slot = u64;
const LINK_CAPACITY: usize = 16384;

pub struct DispatchEndpoints {
    pub transaction_status: TransactionStatusRx,
    pub transaction_scheduler: TransactionSchedulerHandle,
    pub account_update: AccountUpdateRx,
    pub ensure_accounts: EnsureAccountsTx,
    pub block_update: BlockUpdateRx,
}

pub struct ValidatorChannelEndpoints {
    pub transaction_status: TransactionStatusTx,
    pub transaction_to_process: TransactionToProcessRx,
    pub account_update: AccountUpdateTx,
    pub ensure_accounts: EnsureAccountsRx,
    pub block_update: BlockUpdateTx,
}

pub fn link() -> (DispatchEndpoints, ValidatorChannelEndpoints) {
    let (transaction_status_tx, transaction_status_rx) = flume::unbounded();
    let (account_update_tx, account_update_rx) = flume::unbounded();
    let (txn_to_process_tx, txn_to_process_rx) = mpsc::channel(LINK_CAPACITY);
    let (ensure_accounts_tx, ensure_accounts_rx) = mpsc::channel(LINK_CAPACITY);
    let (block_update_tx, block_update_rx) = flume::unbounded();
    let dispatch = DispatchEndpoints {
        transaction_scheduler: TransactionSchedulerHandle(txn_to_process_tx),
        transaction_status: transaction_status_rx,
        account_update: account_update_rx,
        ensure_accounts: ensure_accounts_tx,
        block_update: block_update_rx,
    };
    let validator = ValidatorChannelEndpoints {
        transaction_to_process: txn_to_process_rx,
        transaction_status: transaction_status_tx,
        ensure_accounts: ensure_accounts_rx,
        account_update: account_update_tx,
        block_update: block_update_tx,
    };
    (dispatch, validator)
}
