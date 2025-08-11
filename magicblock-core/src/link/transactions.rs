use flume::{Receiver as MpmcReceiver, Sender as MpmcSender};
use solana_program::message::inner_instruction::InnerInstructionsList;
use solana_pubkey::Pubkey;
use solana_signature::Signature;
use solana_transaction::sanitized::SanitizedTransaction;
use solana_transaction_context::TransactionReturnData;
use tokio::sync::{
    mpsc::{Receiver, Sender},
    oneshot,
};

use crate::Slot;

pub type TxnStatusRx = MpmcReceiver<TransactionStatus>;
pub type TxnStatusTx = MpmcSender<TransactionStatus>;

pub type TxnToProcessRx = Receiver<ProcessableTransaction>;
pub type TxnToProcessTx = Sender<ProcessableTransaction>;

pub type TransactionResult = solana_transaction_error::TransactionResult<()>;
pub type TxnSimulationResultTx = oneshot::Sender<TransactionSimulationResult>;
pub type TxnExecutionResultTx = Option<oneshot::Sender<TransactionResult>>;

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
    Simulation(TxnSimulationResultTx),
    Execution(TxnExecutionResultTx),
}

pub struct TransactionExecutionResult {
    pub result: TransactionResult,
    pub accounts: Box<[Pubkey]>,
    pub logs: Option<Vec<String>>,
}

pub struct TransactionSimulationResult {
    pub result: TransactionResult,
    pub logs: Option<Vec<String>>,
    pub units_consumed: u64,
    pub return_data: Option<TransactionReturnData>,
    pub inner_instructions: Option<InnerInstructionsList>,
}
