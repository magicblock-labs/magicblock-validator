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

use super::blocks::BlockHash;

/// The receiver end of the multi-producer, multi-consumer
/// channel for communicating final transaction statuses.
pub type TransactionStatusRx = MpmcReceiver<TransactionStatus>;
/// The sender end of the multi-producer, multi-consumer
/// channel for communicating final transaction statuses.
pub type TransactionStatusTx = MpmcSender<TransactionStatus>;

/// The receiver end of the channel used to send new transactions to the scheduler for processing.
pub type TransactionToProcessRx = Receiver<ProcessableTransaction>;
/// The sender end of the channel used to send new transactions to the scheduler for processing.
type TransactionToProcessTx = Sender<ProcessableTransaction>;

/// A cloneable handle that provides a high-level API for
/// submitting transactions to the processing pipeline.
///
/// This is the primary entry point for all transaction-related
/// operations like execution, simulation, and replay.
#[derive(Clone)]
pub struct TransactionSchedulerHandle(pub(super) TransactionToProcessTx);

/// The standard result of a transaction execution, indicating success or a `TransactionError`.
pub type TransactionResult = solana_transaction_error::TransactionResult<()>;
/// The sender half of a one-shot channel used to return the result of a transaction simulation.
pub type TxnSimulationResultTx = oneshot::Sender<TransactionSimulationResult>;
/// An optional sender half of a one-shot channel for returning a transaction execution result.
/// `None` is used for "fire-and-forget" scheduling.
pub type TxnExecutionResultTx = Option<oneshot::Sender<TransactionResult>>;
/// The sender half of a one-shot channel used to return the result of a transaction replay.
pub type TxnReplayResultTx = oneshot::Sender<TransactionResult>;

/// Contains the final, committed status of an executed
/// transaction, including its result and metadata.
/// This is the message type that is communicated to subscribers via event processors.
pub struct TransactionStatus {
    pub signature: Signature,
    pub slot: Slot,
    pub result: TransactionExecutionResult,
}

/// An internal message that bundles a sanitized transaction with its requested processing mode.
/// This is the message sent to the transaction scheduler.
pub struct ProcessableTransaction {
    pub transaction: SanitizedTransaction,
    pub mode: TransactionProcessingMode,
}

/// An enum that specifies how a transaction should be processed by the scheduler.
/// Each variant also carries the one-shot sender to return the result to the original caller.
pub enum TransactionProcessingMode {
    /// Process the transaction as a simulation.
    Simulation(TxnSimulationResultTx),
    /// Process the transaction for standard execution.
    Execution(TxnExecutionResultTx),
    /// Replay the transaction against the current state without persistence to the ledger.
    Replay(TxnReplayResultTx),
}

/// The detailed outcome of a standard transaction execution.
pub struct TransactionExecutionResult {
    pub result: TransactionResult,
    pub accounts: Box<[Pubkey]>,
    pub logs: Option<Vec<String>>,
}

/// The detailed outcome of a transaction simulation.
/// Contains extra information not available in a standard
/// execution, like compute units and return data.
pub struct TransactionSimulationResult {
    pub result: TransactionResult,
    pub logs: Option<Vec<String>>,
    pub units_consumed: u64,
    pub return_data: Option<TransactionReturnData>,
    pub inner_instructions: Option<InnerInstructionsList>,
}

/// A trait for transaction types that can be converted into a `SanitizedTransaction`.
///
/// This provides a uniform `sanitize()` method to abstract away the boilerplate of
/// preparing different transaction formats for processing by the SVM.
pub trait SanitizeableTransaction {
    /// Sanitizes the transaction, making it ready for processing.
    ///
    /// Sanitization involves verifying the transaction's structure, hashing its
    /// message, and optionally verifying its signatures.
    ///
    /// # Arguments
    /// * `verify` - If `true`, the transaction's signatures are cryptographically
    ///   verified. This is computationally expensive and can be skipped for certain
    ///   operations like simulations or replays
    ///
    /// # Returns
    /// A `Result` containing the `SanitizedTransaction` on success, or a
    /// `TransactionError` if sanitization fails.
    fn sanitize(
        self,
        verify: bool,
    ) -> Result<SanitizedTransaction, TransactionError>;
}

impl SanitizeableTransaction for SanitizedTransaction {
    fn sanitize(self, _: bool) -> Result<Self, TransactionError> {
        Ok(self)
    }
}

impl SanitizeableTransaction for VersionedTransaction {
    fn sanitize(
        self,
        verify: bool,
    ) -> Result<SanitizedTransaction, TransactionError> {
        println!("verifying transaction: {verify}");
        let hash = if verify {
            self.verify_and_hash_message()
        } else {
            Ok(BlockHash::new_unique())
        }?;
        SanitizedTransaction::try_create(
            self,
            hash,
            Some(false),
            SimpleAddressLoader::Disabled,
            &Default::default(),
        )
    }
}

impl SanitizeableTransaction for Transaction {
    fn sanitize(
        self,
        verify: bool,
    ) -> Result<SanitizedTransaction, TransactionError> {
        VersionedTransaction::from(self).sanitize(verify)
    }
}

impl TransactionSchedulerHandle {
    /// Submits a transaction for "fire-and-forget" execution.
    ///
    /// This method is preferred when the result of the execution is not needed,
    /// as it has lower overhead than `execute()`. It does not wait for the transaction
    /// to be processed.
    pub async fn schedule(
        &self,
        txn: impl SanitizeableTransaction,
    ) -> TransactionResult {
        let transaction = txn.sanitize(true)?;
        let mode = TransactionProcessingMode::Execution(None);
        let txn = ProcessableTransaction { transaction, mode };
        let r = self.0.send(txn).await;
        r.map_err(|_| TransactionError::ClusterMaintenance)
    }

    /// Submits a transaction for execution and asynchronously awaits its result.
    ///
    /// This method has a higher overhead than `schedule()` due to the need
    /// to manage a one-shot channel for the result. Use it when you need
    /// to act upon the transaction's success or failure.
    pub async fn execute(
        &self,
        txn: impl SanitizeableTransaction,
    ) -> TransactionResult {
        let mode = |tx| TransactionProcessingMode::Execution(Some(tx));
        self.send(txn, mode).await?
    }

    /// Submits a transaction for simulation and awaits the detailed simulation result.
    pub async fn simulate(
        &self,
        txn: impl SanitizeableTransaction,
    ) -> Result<TransactionSimulationResult, TransactionError> {
        let mode = TransactionProcessingMode::Simulation;
        self.send(txn, mode).await
    }

    /// Submits a transaction to be replayed against the
    /// current accountsdb state and awaits the result.
    pub async fn replay(
        &self,
        txn: impl SanitizeableTransaction,
    ) -> TransactionResult {
        let mode = TransactionProcessingMode::Replay;
        self.send(txn, mode).await?
    }

    /// A private helper that handles the common logic of sanitizing, sending a
    /// transaction with a one-shot reply channel, and awaiting the response.
    async fn send<R>(
        &self,
        txn: impl SanitizeableTransaction,
        mode: fn(oneshot::Sender<R>) -> TransactionProcessingMode,
    ) -> Result<R, TransactionError> {
        let transaction = txn.sanitize(true)?;
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
