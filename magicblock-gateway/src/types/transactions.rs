use flume::{Receiver as MpmcReceiver, Sender as MpmcSender};
use solana_pubkey::Pubkey;
use solana_signature::Signature;
use solana_transaction::sanitized::SanitizedTransaction;
use solana_transaction_context::TransactionReturnData;
use solana_transaction_status_client_types::InnerInstructions;
use tokio::sync::{
    mpsc::{Receiver, Sender},
    oneshot,
};

use crate::Slot;

pub type TxnStatusRx = MpmcReceiver<TransactionStatus>;
pub type TxnStatusTx = MpmcSender<TransactionStatus>;

pub type TxnExecutionRx = Receiver<ProcessableTransaction>;
pub type TxnExecutionTx = Sender<ProcessableTransaction>;

pub type TransactionResult = solana_transaction_error::TransactionResult<()>;

pub struct TransactionStatus {
    pub signature: Signature,
    pub slot: Slot,
    pub result: TransactionExecutionResult,
}

pub struct ProcessableTransaction {
    pub transaction: SanitizedTransaction,
    pub mode: TransactionProcessingMode,
}

pub enum TransactionProcessingMode {
    Simulation {
        inner_instructions: bool,
        result_tx: oneshot::Sender<TransactionSimulationResult>,
    },
    Execution(Option<oneshot::Sender<TransactionResult>>),
}

pub struct TransactionExecutionResult {
    pub result: TransactionResult,
    pub accounts: Box<[Pubkey]>,
    pub logs: Box<[String]>,
}

pub struct TransactionSimulationResult {
    pub result: TransactionResult,
    pub logs: Box<[String]>,
    pub units_consumed: u64,
    pub return_data: Option<TransactionReturnData>,
    pub inner_instructions: Option<Box<[InnerInstructions]>>,
}
