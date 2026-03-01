use flume::{Receiver as MpmcReceiver, Sender as MpmcSender};
use magicblock_magic_program_api::args::TaskRequest;
use serde::Serialize;
use solana_program::message::{
    inner_instruction::InnerInstructionsList, SimpleAddressLoader,
};
use solana_transaction::{
    sanitized::SanitizedTransaction, versioned::VersionedTransaction,
    Transaction,
};
use solana_transaction_context::TransactionReturnData;
use solana_transaction_error::TransactionError;
use solana_transaction_status_client_types::TransactionStatusMeta;
use tokio::sync::{
    mpsc::{Receiver, Sender, UnboundedReceiver, UnboundedSender},
    oneshot,
};

use super::blocks::BlockHash;
use crate::Slot;

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

/// The receiver end of the channel used to send scheduled tasks (cranking)
pub type ScheduledTasksRx = UnboundedReceiver<TaskRequest>;
/// The sender end of the channel used to send scheduled tasks (cranking)
pub type ScheduledTasksTx = UnboundedSender<TaskRequest>;

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
#[derive(Debug)]
pub struct TransactionStatus {
    pub slot: Slot,
    pub txn: SanitizedTransaction,
    pub meta: TransactionStatusMeta,
    pub index: u32,
}

/// An internal message that bundles a sanitized transaction with its requested processing mode.
/// This is the message sent to the transaction scheduler.
pub struct ProcessableTransaction {
    pub transaction: SanitizedTransaction,
    pub mode: TransactionProcessingMode,
    /// Pre-encoded bincode bytes for the transaction.
    /// Used by the replicator to avoid redundant serialization.
    pub encoded: Option<Vec<u8>>,
}

/// An enum that specifies how a transaction should be processed by the scheduler.
///
/// Variants that require result notification carry a one-shot sender:
/// - `Simulation` and `Execution` return results to the caller
/// - `Replay` is fire-and-forget (no sender, just a persistence flag)
pub enum TransactionProcessingMode {
    /// Process the transaction as a simulation.
    Simulation(TxnSimulationResultTx),
    /// Process the transaction for standard execution.
    Execution(TxnExecutionResultTx),
    /// Replay the transaction against the current state (fire-and-forget).
    ///
    /// The `bool` flag controls ledger persistence:
    /// - `true`: record to ledger and broadcast status (for replay from primary)
    /// - `false`: no recording, no broadcast (for local ledger replay during startup)
    Replay(bool),
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

    /// Sanitizes the transaction and optionally provides pre-encoded bincode bytes.
    ///
    /// Default implementation delegates to `sanitize()` and returns `None` for encoded bytes.
    /// Override this method when you have pre-encoded bytes (e.g., from the wire) to avoid
    /// redundant serialization.
    fn sanitize_with_encoded(
        self,
        verify: bool,
    ) -> Result<(SanitizedTransaction, Option<Vec<u8>>), TransactionError>
    where
        Self: Sized,
    {
        let txn = self.sanitize(verify)?;
        Ok((txn, None))
    }
}

/// Wraps a transaction with its pre-encoded bincode representation.
/// Use for internally-constructed transactions that need encoded bytes.
pub struct WithEncoded<T> {
    pub txn: T,
    pub encoded: Vec<u8>,
}

impl<T: SanitizeableTransaction> SanitizeableTransaction for WithEncoded<T> {
    fn sanitize(
        self,
        verify: bool,
    ) -> Result<SanitizedTransaction, TransactionError> {
        self.txn.sanitize(verify)
    }

    fn sanitize_with_encoded(
        self,
        verify: bool,
    ) -> Result<(SanitizedTransaction, Option<Vec<u8>>), TransactionError> {
        let txn = self.txn.sanitize(verify)?;
        Ok((txn, Some(self.encoded)))
    }
}

/// Encodes a transaction to bincode and wraps it with its encoded form.
/// Use for internally-constructed transactions that need the encoded bytes.
pub fn with_encoded<T>(txn: T) -> Result<WithEncoded<T>, TransactionError>
where
    T: Serialize,
{
    let encoded = bincode::serialize(&txn)
        .map_err(|_| TransactionError::SanitizeFailure)?;
    Ok(WithEncoded { txn, encoded })
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
        let hash = if verify {
            self.verify_and_hash_message()
        } else {
            Ok(BlockHash::new_unique())
        }?;
        SanitizedTransaction::try_create(
            self,
            hash,
            Some(false),
            SimpleAddressLoader::Enabled(Default::default()),
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
        let (transaction, encoded) = txn.sanitize_with_encoded(true)?;
        let mode = TransactionProcessingMode::Execution(None);
        let txn = ProcessableTransaction {
            transaction,
            mode,
            encoded,
        };
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

    /// Submits a transaction to be replayed against the current accountsdb state.
    ///
    /// Unlike `execute()`, this method is fire-and-forget: it returns success
    /// once the transaction is queued, not after execution completes.
    ///
    /// # Arguments
    /// * `persist` - If true, record the transaction to the ledger
    /// * `txn` - The transaction to replay
    pub async fn replay(
        &self,
        persist: bool,
        txn: impl SanitizeableTransaction,
    ) -> TransactionResult {
        let mode = TransactionProcessingMode::Replay(persist);
        let transaction = txn.sanitize(true)?;
        let txn = ProcessableTransaction {
            transaction,
            mode,
            encoded: None,
        };
        self.0
            .send(txn)
            .await
            .map_err(|_| TransactionError::ClusterMaintenance)
    }

    /// A private helper that handles the common logic of sanitizing, sending a
    /// transaction with a one-shot reply channel, and awaiting the response.
    async fn send<R>(
        &self,
        txn: impl SanitizeableTransaction,
        mode: fn(oneshot::Sender<R>) -> TransactionProcessingMode,
    ) -> Result<R, TransactionError> {
        let (transaction, encoded) = txn.sanitize_with_encoded(true)?;
        let (tx, rx) = oneshot::channel();
        let mode = mode(tx);
        let txn = ProcessableTransaction {
            transaction,
            mode,
            encoded,
        };
        self.0
            .send(txn)
            .await
            .map_err(|_| TransactionError::ClusterMaintenance)?;
        rx.await.map_err(|_| TransactionError::ClusterMaintenance)
    }
}
