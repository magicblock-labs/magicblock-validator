use accounts::{
    AccountUpdateRx, AccountUpdateTx, EnsureAccountsRx, EnsureAccountsTx,
};
use blocks::{BlockUpdateRx, BlockUpdateTx};
use tokio::sync::mpsc;
use transactions::{TxnStatusRx, TxnStatusTx, TxnToProcessRx, TxnToProcessTx};

pub mod accounts;
pub mod blocks;
pub mod transactions;

pub type Slot = u64;
const LINK_CAPACITY: usize = 16384;

pub struct RpcChannelEndpoints {
    pub transaction_status_rx: TxnStatusRx,
    pub processable_txn_tx: TxnToProcessTx,
    pub account_update_rx: AccountUpdateRx,
    pub ensure_accounts_tx: EnsureAccountsTx,
    pub block_update_rx: BlockUpdateRx,
}

pub struct ValidatorChannelEndpoints {
    pub transaction_status_tx: TxnStatusTx,
    pub processable_txn_rx: TxnToProcessRx,
    pub account_update_tx: AccountUpdateTx,
    pub ensure_accounts_rx: EnsureAccountsRx,
    pub block_update_tx: BlockUpdateTx,
}

pub fn link() -> (RpcChannelEndpoints, ValidatorChannelEndpoints) {
    let (transaction_status_tx, transaction_status_rx) = flume::unbounded();
    let (account_update_tx, account_update_rx) = flume::unbounded();
    let (processable_txn_tx, processable_txn_rx) = mpsc::channel(LINK_CAPACITY);
    let (ensure_accounts_tx, ensure_accounts_rx) = mpsc::channel(LINK_CAPACITY);
    let (block_update_tx, block_update_rx) = flume::unbounded();
    let rpc = RpcChannelEndpoints {
        processable_txn_tx,
        transaction_status_rx,
        account_update_rx,
        ensure_accounts_tx,
        block_update_rx,
    };
    let validator = ValidatorChannelEndpoints {
        processable_txn_rx,
        transaction_status_tx,
        ensure_accounts_rx,
        account_update_tx,
        block_update_tx,
    };
    (rpc, validator)
}
