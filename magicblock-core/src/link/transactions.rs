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

pub type TransactionStatusRx = MpmcReceiver<TransactionStatus>;
pub type TransactionStatusTx = MpmcSender<TransactionStatus>;

pub type TransactionToProcessRx = Receiver<ProcessableTransaction>;
type TransactionToProcessTx = Sender<ProcessableTransaction>;

/// Convenience wrapper around channel endpoint to global transaction scheduler
#[derive(Clone)]
pub struct TransactionSchedulerHandle(pub(super) TransactionToProcessTx);

pub type TransactionResult = solana_transaction_error::TransactionResult<()>;
pub type TxnSimulationResultTx = oneshot::Sender<TransactionSimulationResult>;
pub type TxnExecutionResultTx = Option<oneshot::Sender<TransactionResult>>;
pub type TxnReplayResultTx = oneshot::Sender<TransactionResult>;

/// Status of executed transaction along with some metadata
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
    Replay(TxnReplayResultTx),
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

impl TransactionSchedulerHandle {
    /// Fire and forget the transaction for execution
    pub async fn schedule(&self, transaction: SanitizedTransaction) {
        let txn = ProcessableTransaction {
            transaction,
            mode: TransactionProcessingMode::Execution(None),
        };
        let _ = self.0.send(txn).await;
    }

    /// Send the transaction for execution and await for result
    pub async fn execute(
        &self,
        transaction: SanitizedTransaction,
    ) -> Option<TransactionResult> {
        let (tx, rx) = oneshot::channel();
        let txn = ProcessableTransaction {
            transaction,
            mode: TransactionProcessingMode::Execution(Some(tx)),
        };
        self.0.send(txn).await.ok()?;
        rx.await.ok()
    }

    /// Send transaction for simulation and await for result
    pub async fn simulate(
        &self,
        transaction: SanitizedTransaction,
    ) -> Option<TransactionSimulationResult> {
        let (tx, rx) = oneshot::channel();
        let txn = ProcessableTransaction {
            transaction,
            mode: TransactionProcessingMode::Simulation(tx),
        };
        self.0.send(txn).await.ok()?;
        rx.await.ok()
    }

    /// Send transaction to be replayed on top of
    /// existing account state and wait for result
    pub async fn replay(
        &self,
        transaction: SanitizedTransaction,
    ) -> Option<TransactionResult> {
        let (tx, rx) = oneshot::channel();
        let txn = ProcessableTransaction {
            transaction,
            mode: TransactionProcessingMode::Replay(tx),
        };
        self.0.send(txn).await.ok()?;
        rx.await.ok()
    }
}
