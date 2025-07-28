use solana_message::inner_instruction::InnerInstructions;
use solana_pubkey::Pubkey;
use solana_signature::Signature;
use solana_transaction::versioned::VersionedTransaction;
use solana_transaction_context::{TransactionAccount, TransactionReturnData};
use tokio::sync::{
    mpsc::{Receiver, Sender},
    oneshot,
};

pub use solana_transaction_error::TransactionError;

pub type TxnStatusRx = Receiver<TransactionStatus>;
pub type TxnStatusTx = Sender<TransactionStatus>;

pub type TxnExecutionRx = Receiver<ProcessableTransaction>;
pub type TxnExecutionTx = Sender<ProcessableTransaction>;

pub type TxnResultRx = Receiver<TransactionProcessingResult>;
pub type TxnResultTx = Sender<TransactionProcessingResult>;

pub type TransactionResult = solana_transaction_error::TransactionResult<()>;

pub struct TransactionStatus {
    pub signature: Signature,
    pub result: TransactionProcessingResult,
}

pub struct ProcessableTransaction {
    pub transaction: VersionedTransaction,
    pub simulate: bool,
    pub result_tx: Option<oneshot::Sender<TransactionResult>>,
}

pub enum TransactionProcessingResult {
    Execution(TransactionExecutionResult),
    Simulation(TransactionSimulationResult),
}

pub struct TransactionExecutionResult {
    pub result: TransactionResult,
    pub accounts: Box<[Pubkey]>,
    pub logs: Box<[String]>,
}

pub struct TransactionSimulationResult {
    pub result: TransactionResult,
    pub logs: Box<[String]>,
    pub post_simulation_accounts: Box<[TransactionAccount]>,
    pub units_consumed: u64,
    pub return_data: Option<Box<TransactionReturnData>>,
    pub inner_instructions: Option<Box<[InnerInstructions]>>,
}
