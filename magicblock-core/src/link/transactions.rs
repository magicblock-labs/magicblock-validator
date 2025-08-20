use flume::{Receiver as MpmcReceiver, Sender as MpmcSender};
use solana_program::message::{
    inner_instruction::InnerInstructionsList, SimpleAddressLoader,
};
use solana_pubkey::Pubkey;
use solana_signature::Signature;
use solana_transaction::{
    sanitized::SanitizedTransaction, versioned::VersionedTransaction,
    Transaction,
};
use solana_transaction_context::TransactionReturnData;
use solana_transaction_error::TransactionError;
use tokio::sync::{
    mpsc::{Receiver, Sender},
    oneshot,
};

use crate::Slot;

pub type TransactionStatusRx = MpmcReceiver<TransactionStatus>;
pub type TransactionStatusTx = MpmcSender<TransactionStatus>;

pub type TransactionToProcessRx = Receiver<ProcessableTransaction>;
type TransactionToProcessTx = Sender<ProcessableTransaction>;

/// Convenience wrapper around channel endpoint to the global (internal)
/// transaction scheduler - single entrypoint for transaction execution
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

/// Opt in convenience trait, which can be used to send transactions for
/// execution without the sanitization boilerplate. In case if the sanitization
/// result is important, which is rarely the case for transactions originating
/// internally, the `SanitizeableTransaction::sanitize` can invoked directly
/// before sending the transaction for execution/replay
pub trait SanitizeableTransaction {
    fn sanitize(self) -> Result<SanitizedTransaction, TransactionError>;
}

impl SanitizeableTransaction for SanitizedTransaction {
    fn sanitize(self) -> Result<Self, TransactionError> {
        Ok(self)
    }
}

impl SanitizeableTransaction for VersionedTransaction {
    fn sanitize(self) -> Result<SanitizedTransaction, TransactionError> {
        let hash = self.verify_and_hash_message()?;
        SanitizedTransaction::try_create(
            self,
            hash,
            None,
            SimpleAddressLoader::Disabled,
            &Default::default(),
        )
    }
}

impl SanitizeableTransaction for Transaction {
    fn sanitize(self) -> Result<SanitizedTransaction, TransactionError> {
        VersionedTransaction::from(self).sanitize()
    }
}

impl TransactionSchedulerHandle {
    /// Fire and forget the transaction for execution
    pub async fn schedule(
        &self,
        txn: impl SanitizeableTransaction,
    ) -> Result<(), TransactionError> {
        let transaction = txn.sanitize()?;
        let mode = TransactionProcessingMode::Execution(None);
        let txn = ProcessableTransaction { transaction, mode };
        let r = self.0.send(txn).await;
        r.map_err(|_| TransactionError::ClusterMaintenance)
    }

    /// Send the transaction for execution and await for result
    pub async fn execute(
        &self,
        txn: impl SanitizeableTransaction,
    ) -> TransactionResult {
        let mode = |tx| TransactionProcessingMode::Execution(Some(tx));
        self.send(txn, mode).await?
    }

    /// Send transaction for simulation and await for result
    pub async fn simulate(
        &self,
        txn: impl SanitizeableTransaction,
    ) -> Result<TransactionSimulationResult, TransactionError> {
        let mode = TransactionProcessingMode::Simulation;
        self.send(txn, mode).await
    }

    /// Send transaction to be replayed on top of
    /// existing account state and wait for result
    pub async fn replay(
        &self,
        txn: impl SanitizeableTransaction,
    ) -> TransactionResult {
        let mode = TransactionProcessingMode::Replay;
        self.send(txn, mode).await?
    }

    /// Sanitize and send transaction for processing and await for result
    async fn send<R>(
        &self,
        txn: impl SanitizeableTransaction,
        mode: fn(oneshot::Sender<R>) -> TransactionProcessingMode,
    ) -> Result<R, TransactionError> {
        let transaction = txn.sanitize()?;
        let (tx, rx) = oneshot::channel();
        let mode = mode(tx);
        let txn = ProcessableTransaction { transaction, mode };
        self.0
            .send(txn)
            .await
            .map_err(|_| TransactionError::ClusterMaintenance)?;
        rx.await.map_err(|_| TransactionError::ClusterMaintenance)
    }
}
