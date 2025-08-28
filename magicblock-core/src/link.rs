use accounts::{AccountUpdateRx, AccountUpdateTx};
use blocks::{BlockUpdateRx, BlockUpdateTx};
use tokio::sync::mpsc;
use transactions::{
    TransactionSchedulerHandle, TransactionStatusRx, TransactionStatusTx,
    TransactionToProcessRx,
};

pub mod accounts;
pub mod blocks;
pub mod transactions;

/// The bounded capacity for MPSC channels that require backpressure.
const LINK_CAPACITY: usize = 16384;

/// A collection of channel endpoints for the **dispatch side** of the validator.
///
/// This struct is the primary interface for external-facing components (like the
/// HTTP and WebSocket servers) to interact with the validator's internal core.
/// It allows them to send commands *to* the core and receive broadcasted updates *from* it.
pub struct DispatchEndpoints {
    /// Receives the final status of processed transactions from the executor.
    pub transaction_status: TransactionStatusRx,
    /// Sends new transactions to the executor to be scheduled for processing.
    pub transaction_scheduler: TransactionSchedulerHandle,
    /// Receives notifications about account state changes from the executor.
    pub account_update: AccountUpdateRx,
    /// Receives notifications when a new block is produced.
    pub block_update: BlockUpdateRx,
}

/// A collection of channel endpoints for the **validator's internal core**.
///
/// This struct is the interface for the internal machinery (e.g., `TransactionExecutor`,
/// `BlockProducer`) to receive commands from the dispatch side and to broadcast
/// updates to all listeners.
pub struct ValidatorChannelEndpoints {
    /// Sends the final status of processed transactions to the pool of EventProccessor workers.
    pub transaction_status: TransactionStatusTx,
    /// Receives new transactions from the dispatch side to be processed.
    pub transaction_to_process: TransactionToProcessRx,
    /// Sends notifications about account state changes to the pool of EventProccessor workers.
    pub account_update: AccountUpdateTx,
    /// Sends notifications when a new block is produced to the pool of EventProcessor workers.
    pub block_update: BlockUpdateTx,
}

/// Creates and connects the full set of communication channels between the dispatch
/// layer and the validator core.
///
/// # Returns
///
/// A tuple containing:
/// 1.  `DispatchEndpoints` for the "client" side (e.g., RPC servers).
/// 2.  `ValidatorChannelEndpoints` for the "server" side (e.g., the transaction executor).
pub fn link() -> (DispatchEndpoints, ValidatorChannelEndpoints) {
    // Unbounded channels for high-throughput broadcasts where backpressure is not desired.
    let (transaction_status_tx, transaction_status_rx) = flume::unbounded();
    let (account_update_tx, account_update_rx) = flume::unbounded();
    let (block_update_tx, block_update_rx) = flume::unbounded();

    // Bounded channels for command queues where applying backpressure is important.
    let (txn_to_process_tx, txn_to_process_rx) = mpsc::channel(LINK_CAPACITY);

    // Bundle the respective channel ends for the dispatch side.
    let dispatch = DispatchEndpoints {
        transaction_scheduler: TransactionSchedulerHandle(txn_to_process_tx),
        transaction_status: transaction_status_rx,
        account_update: account_update_rx,
        block_update: block_update_rx,
    };

    // Bundle the corresponding channel ends for the validator's internal core.
    let validator = ValidatorChannelEndpoints {
        transaction_to_process: txn_to_process_rx,
        transaction_status: transaction_status_tx,
        account_update: account_update_tx,
        block_update: block_update_tx,
    };

    (dispatch, validator)
}
