use solana_message::inner_instruction::InnerInstructions;
use solana_signature::Signature;
use solana_transaction::versioned::VersionedTransaction;
use solana_transaction_context::{TransactionAccount, TransactionReturnData};
use tokio::sync::{
    mpsc::{Receiver, Sender},
    oneshot,
};

pub struct TransactionStatus {
    pub signature: Signature,
    pub success: bool,
}

pub struct ProcessableTransaction {
    pub transaction: VersionedTransaction,
    pub simulate: bool,
    pub result_tx: Option<oneshot::Sender<TransactionResult>>,
}

pub enum TransactionProcessingResult {
    Execution(TransactionResult),
    Simulation(Box<TransactionSimulationResult>),
}

pub struct TransactionSimulationResult {
    pub result: TransactionResult,
    pub logs: Vec<String>,
    pub post_simulation_accounts: Vec<TransactionAccount>,
    pub units_consumed: u64,
    pub return_data: Option<TransactionReturnData>,
    pub inner_instructions: Option<Vec<InnerInstructions>>,
}

pub type TxnStatusRx = Receiver<TransactionStatus>;
pub type TxnStatusTx = Sender<TransactionStatus>;

pub type TxnExecutionRx = Receiver<ProcessableTransaction>;
pub type TxnExecutionTx = Sender<ProcessableTransaction>;

pub type TxnResultRx = Receiver<TransactionProcessingResult>;
pub type TxnResultTx = Sender<TransactionProcessingResult>;

pub type TransactionResult = solana_transaction_error::TransactionResult<()>;
