use mbv_core::traits::ActionError;
use metrics::metrics;
use rpc_client::MagicBlockRpcClientError;
use solana_message::CompileError;
use solana_signature::Signature;
use solana_signer::SignerError;

use crate::{
    intent_engine::intent_scheduler::IntentSchedulerError,
    intent_executor::strategy_executor::error::TransactionExecutionError,
    outbox::outbox_client::InternalOutboxClientError,
    tasks::{task_builder::TaskBuilderError, task_strategist::TaskStrategistError},
    transaction_preparator::error::TransactionPreparatorError,
};

#[derive(thiserror::Error, Debug)]
pub enum InternalError {
    #[error("SignerError: {0}")]
    SignerError(#[from] SignerError),
    #[error("MagicBlockRpcClientError: {0}")]
    MagicBlockRpcClientError(Box<MagicBlockRpcClientError>),
}

impl From<MagicBlockRpcClientError> for InternalError {
    fn from(e: MagicBlockRpcClientError) -> Self {
        Self::MagicBlockRpcClientError(Box::new(e))
    }
}

impl InternalError {
    pub fn signature(&self) -> Option<Signature> {
        match self {
            Self::SignerError(_) => None,
            Self::MagicBlockRpcClientError(err) => err.signature(),
        }
    }

    /// True when the failure is plausibly transient (transport, RPC
    /// availability) and a re-send may succeed.
    pub fn is_transient(&self) -> bool {
        match self {
            Self::SignerError(_) => false,
            Self::MagicBlockRpcClientError(err) => err.is_transient(),
        }
    }
}

#[derive(thiserror::Error, Debug)]
pub enum IntentExecutorError {
    #[error("Intent scheduler state is inconsistent: {0}")]
    SchedulerError(#[from] IntentSchedulerError),
    #[error("EmptyIntentError")]
    EmptyIntentError,
    #[error("Failed to fit in single TX")]
    FailedToFitError,
    #[error("Failed to compile transaction: {0}")]
    CompileError(#[from] CompileError),
    /// This doesn't mean permanent failure
    /// This means that some ancestor failed
    /// to execute with multiple retries
    #[error("Intent got poisoned")]
    PoisonedIntentError,
    #[error("SignerError: {0}")]
    SignerError(#[from] SignerError),
    #[error("OutboxClientError: {0}")]
    OutboxClientError(#[from] InternalOutboxClientError),
    #[error("Failed to get pending signature status: {0}")]
    GetPendingSignatureStatusError(#[source] MagicBlockRpcClientError),
    #[error("Failed to resolve pending signature: {0}")]
    PendingSignatureResolutionError(Signature),
    #[error("TaskBuilderError: {0}")]
    TaskBuilderError(#[from] TaskBuilderError),
    #[error("Transaction execution failed: {0}")]
    FailedExecution(#[from] TransactionExecutionError),
    #[error("Transaction preparation failed: {0}")]
    FailedPreparation(#[from] TransactionPreparatorError),
}

impl IntentExecutorError {
    /// Whether the cause is transient. Execution progress separately determines
    /// whether a fresh attempt is safe.
    pub fn is_transient(&self) -> bool {
        match self {
            Self::GetPendingSignatureStatusError(err) => err.is_transient(),
            Self::TaskBuilderError(err) => err.is_transient(),
            Self::FailedExecution(err) => err.is_transient(),
            Self::FailedPreparation(err) => err.is_transient(),
            _ => false,
        }
    }

    /// A transaction signature reported by the underlying failure. Confirmed
    /// commits and locally prepared/submitted signatures live in ExecutionProgress.
    pub fn signature(&self) -> Option<Signature> {
        match self {
            Self::FailedExecution(err) => err.signature(),
            Self::FailedPreparation(err) => err.signature(),
            Self::TaskBuilderError(err) => err.signature(),
            Self::PendingSignatureResolutionError(signature) => Some(*signature),
            _ => None,
        }
    }
}

impl metrics::LabelValue for IntentExecutorError {
    fn value(&self) -> &str {
        match self {
            Self::FailedExecution(err) => err.value(),
            _ => "failed",
        }
    }
}

impl From<&IntentExecutorError> for ActionError {
    fn from(value: &IntentExecutorError) -> Self {
        match value {
            IntentExecutorError::FailedExecution(err) => err.into(),
            err => ActionError::IntentFailedError(err.to_string()),
        }
    }
}

impl From<TaskStrategistError> for IntentExecutorError {
    fn from(value: TaskStrategistError) -> Self {
        match value {
            TaskStrategistError::FailedToFitError => Self::FailedToFitError,
            TaskStrategistError::CompileError(err) => Self::CompileError(err),
            TaskStrategistError::SignerError(err) => Self::SignerError(err),
        }
    }
}
pub type IntentExecutorResult<T, E = IntentExecutorError> = Result<T, E>;

#[cfg(test)]
mod tests {
    use rpc_client::MagicBlockRpcClientError;
    use solana_rpc_client_api::{
        client_error::{Error as RpcClientError, ErrorKind as RpcClientErrorKind},
        request::RpcRequest,
    };

    use super::InternalError;
    use crate::intent_executor::{
        strategy_executor::error::TransactionExecutionError, test_utils::send_transaction_error,
    };

    #[test]
    fn transport_errors_are_transient() {
        let err = TransactionExecutionError::InternalError(
            send_transaction_error("connection reset").into(),
        );
        assert!(err.is_transient());

        let err =
            TransactionExecutionError::InternalError(InternalError::MagicBlockRpcClientError(
                Box::new(MagicBlockRpcClientError::SentTransactionError(
                    solana_transaction_error::TransactionError::BlockhashNotFound,
                    solana_signature::Signature::default(),
                )),
            ));
        assert!(err.is_transient());
    }

    #[test]
    fn deterministic_errors_are_not_transient() {
        // Landed on-chain with an instruction error - deterministic
        let err =
            TransactionExecutionError::InternalError(InternalError::MagicBlockRpcClientError(
                Box::new(MagicBlockRpcClientError::SentTransactionError(
                    solana_transaction_error::TransactionError::InstructionError(
                        2,
                        solana_instruction::error::InstructionError::Custom(1),
                    ),
                    solana_signature::Signature::default(),
                )),
            ));
        assert!(!err.is_transient());

        let err = TransactionExecutionError::InternalError(InternalError::SignerError(
            solana_signer::SignerError::Custom("oops".to_string()),
        ));
        assert!(!err.is_transient());

        assert!(!super::IntentExecutorError::EmptyIntentError.is_transient());
        assert!(!super::IntentExecutorError::FailedToFitError.is_transient());
    }

    #[test]
    fn builder_and_preparation_error_transience() {
        use crate::{
            tasks::{task_builder::TaskBuilderError, task_info_fetcher::TaskInfoFetcherError},
            transaction_preparator::{
                delivery_preparator::{
                    DeliveryPreparatorError, InternalError as DeliveryInternalError,
                },
                error::TransactionPreparatorError,
            },
        };

        fn transient_rpc_client_error() -> MagicBlockRpcClientError {
            MagicBlockRpcClientError::SendTransaction(Box::new(RpcClientError {
                request: Some(RpcRequest::SendTransaction),
                kind: Box::new(RpcClientErrorKind::Custom("io".to_string())),
            }))
        }
        fn transient_delivery_error() -> TransactionPreparatorError {
            TransactionPreparatorError::from(DeliveryPreparatorError::FailedToCreateALTError(
                DeliveryInternalError::MagicBlockRpcClientError(Box::new(
                    transient_rpc_client_error(),
                )),
            ))
        }

        // RPC-side commit-id fetch failures are transient
        let err =
            super::IntentExecutorError::TaskBuilderError(TaskBuilderError::CommitTasksBuildError(
                TaskInfoFetcherError::MagicBlockRpcClientError(Box::new(
                    transient_rpc_client_error(),
                )),
            ));
        assert!(err.is_transient());

        // Not-found on fetch is transient (may be a stale RPC read)
        let err = super::IntentExecutorError::TaskBuilderError(
            TaskBuilderError::FollowUpTasksBuildError(TaskInfoFetcherError::AccountNotFoundError(
                solana_pubkey::Pubkey::new_unique(),
            )),
        );
        assert!(err.is_transient());

        // A delegation created after the committed snapshot is terminal
        let err = super::IntentExecutorError::TaskBuilderError(
            TaskBuilderError::FollowUpTasksBuildError(
                TaskInfoFetcherError::DelegationSessionChangedError {
                    delegated_account: solana_pubkey::Pubkey::new_unique(),
                    delegation_slot: 2,
                    snapshot_slot: 1,
                },
            ),
        );
        assert!(!err.is_transient());

        // Missing delegation metadata is deterministic
        let err = super::IntentExecutorError::TaskBuilderError(
            TaskBuilderError::MissingDelegationMetadata(solana_pubkey::Pubkey::new_unique()),
        );
        assert!(!err.is_transient());

        // Delivery preparation RPC failures have a transient cause.
        let err = super::IntentExecutorError::FailedPreparation(transient_delivery_error());
        assert!(err.is_transient());

        // Oversized strategies are deterministic
        let err = super::IntentExecutorError::FailedPreparation(
            TransactionPreparatorError::FailedToFitError,
        );
        assert!(!err.is_transient());
    }

    #[test]
    fn execution_progress_prevents_replaying_confirmed_or_pending_transactions() {
        use crate::intent_executor::{
            IntentExecutionResult,
            progress::{ExecutionProgress, TransactionProgress},
        };
        use solana_signature::Signature;

        let transient_err = || {
            TransactionExecutionError::InternalError(
                send_transaction_error("connection reset").into(),
            )
        };

        let signature = Signature::new_unique();
        for (transaction, confirmed_commit, retry) in [
            (TransactionProgress::Preparing, None, true),
            (TransactionProgress::Prepared(signature), None, true),
            (TransactionProgress::Rejected(signature), None, true),
            (TransactionProgress::Submitted(signature), None, false),
            (TransactionProgress::Confirmed(signature), None, false),
            (TransactionProgress::Preparing, Some(signature), false),
        ] {
            let result = IntentExecutionResult {
                inner: Err(super::IntentExecutorError::FailedExecution(transient_err())),
                progress: ExecutionProgress { transaction, confirmed_commit },
                patched_errors: vec![],
                callbacks_report: vec![],
            };
            assert_eq!(result.is_retriable(true), retry);
            assert!(
                !result.is_retriable(false),
                "action-only execution failures must not get fresh retries"
            );
        }
    }
}
