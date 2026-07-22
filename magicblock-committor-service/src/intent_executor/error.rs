use magicblock_core::traits::ActionError;
use magicblock_metrics::metrics;
use magicblock_rpc_client::MagicBlockRpcClientError;
use solana_signature::Signature;
use solana_signer::SignerError;

use crate::{
    intent_executor::strategy_executor::error::TransactionStrategyExecutionError,
    outbox::outbox_client::InternalOutboxClientError,
    tasks::{
        task_builder::TaskBuilderError, task_strategist::TaskStrategistError,
    },
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

    pub fn is_transaction_too_large(&self) -> bool {
        match self {
            Self::MagicBlockRpcClientError(err) => {
                err.is_transaction_too_large()
            }
            Self::SignerError(_) => false,
        }
    }
}

#[derive(thiserror::Error, Debug)]
pub enum IntentExecutorError {
    #[error("EmptyIntentError")]
    EmptyIntentError,
    #[error("Failed to fit in single TX")]
    FailedToFitError,
    #[error("SignerError: {0}")]
    SignerError(#[from] SignerError),
    #[error("OutboxClientError: {0}")]
    OutboxClientError(#[from] InternalOutboxClientError),
    #[error("Failed to get pending signature status: {0}")]
    GetPendingSignatureStatusError(#[source] MagicBlockRpcClientError),
    #[error("TaskBuilderError: {0}")]
    TaskBuilderError(#[from] TaskBuilderError),
    #[error("FailedToCommitError: {err}")]
    FailedToCommitError {
        #[source]
        err: TransactionStrategyExecutionError,
        signature: Option<Signature>,
    },
    #[error("FailedToFinalizeError: {err}")]
    FailedToFinalizeError {
        #[source]
        err: TransactionStrategyExecutionError,
        commit_signature: Option<Signature>,
        finalize_signature: Option<Signature>,
    },
    #[error("FailedCommitPreparationError: {0}")]
    FailedCommitPreparationError(#[source] TransactionPreparatorError),
    #[error("FailedFinalizePreparationError: {0}")]
    FailedFinalizePreparationError(#[source] TransactionPreparatorError),
}

impl IntentExecutorError {
    pub fn from_commit_execution_error(
        error: TransactionStrategyExecutionError,
    ) -> IntentExecutorError {
        let signature = error.signature();
        IntentExecutorError::FailedToCommitError {
            err: error,
            signature,
        }
    }

    pub fn from_finalize_execution_error(
        error: TransactionStrategyExecutionError,
        commit_signature: Option<Signature>,
    ) -> IntentExecutorError {
        let finalize_signature = error.signature();
        IntentExecutorError::FailedToFinalizeError {
            err: error,
            commit_signature,
            finalize_signature,
        }
    }

    /// True when re-executing the whole intent from scratch may succeed:
    /// the failure was transport/RPC-side rather than deterministic.
    /// Once a commit landed (two-stage finalize failures) re-execution would
    /// commit the same state again, so those are never transient.
    pub fn is_transient(&self) -> bool {
        match self {
            Self::EmptyIntentError
            | Self::FailedToFitError
            | Self::SignerError(_)
            | Self::OutboxClientError(_) => false,
            Self::GetPendingSignatureStatusError(err) => err.is_transient(),
            Self::TaskBuilderError(err) => err.is_transient(),
            Self::FailedToCommitError { err, .. } => err.is_transient(),
            Self::FailedToFinalizeError {
                err,
                commit_signature: None,
                ..
            } => err.is_transient(),
            Self::FailedToFinalizeError { .. }
            | Self::FailedFinalizePreparationError(_) => false,
            Self::FailedCommitPreparationError(err) => err.is_transient(),
        }
    }

    /// Returns signatures of transaction sent to Base layer
    pub fn base_signatures(&self) -> Option<(Signature, Option<Signature>)> {
        match self {
            IntentExecutorError::FailedToCommitError { signature, err: _ } => {
                signature.map(|el| (el, None))
            }
            IntentExecutorError::FailedCommitPreparationError(err)
            | IntentExecutorError::FailedFinalizePreparationError(err) => {
                err.signature().map(|el| (el, None))
            }
            IntentExecutorError::TaskBuilderError(err) => {
                err.signature().map(|el| (el, None))
            }
            IntentExecutorError::FailedToFinalizeError {
                err: _,
                commit_signature,
                finalize_signature,
            } => commit_signature.map(|el| (el, *finalize_signature)),
            IntentExecutorError::EmptyIntentError
            | IntentExecutorError::FailedToFitError
            | IntentExecutorError::SignerError(_)
            | IntentExecutorError::OutboxClientError(_)
            | IntentExecutorError::GetPendingSignatureStatusError(_) => None,
        }
    }
}

impl metrics::LabelValue for IntentExecutorError {
    fn value(&self) -> &str {
        match self {
            IntentExecutorError::FailedToCommitError { err, signature: _ } => {
                err.value()
            }
            IntentExecutorError::FailedToFinalizeError {
                err,
                commit_signature: _,
                finalize_signature: _,
            } => err.value(),
            _ => "failed",
        }
    }
}

impl From<&IntentExecutorError> for ActionError {
    fn from(value: &IntentExecutorError) -> Self {
        match value {
            IntentExecutorError::FailedToCommitError { err, .. }
            | IntentExecutorError::FailedToFinalizeError { err, .. } => {
                err.into()
            }
            err => ActionError::IntentFailedError(err.to_string()),
        }
    }
}

impl From<TaskStrategistError> for IntentExecutorError {
    fn from(value: TaskStrategistError) -> Self {
        match value {
            TaskStrategistError::FailedToFitError => Self::FailedToFitError,
            TaskStrategistError::SignerError(err) => Self::SignerError(err),
        }
    }
}
pub type IntentExecutorResult<T, E = IntentExecutorError> = Result<T, E>;

#[cfg(test)]
mod tests {
    use magicblock_rpc_client::MagicBlockRpcClientError;
    use solana_rpc_client_api::{
        client_error::{
            Error as RpcClientError, ErrorKind as RpcClientErrorKind,
        },
        request::{RpcError, RpcRequest, RpcResponseErrorData},
    };

    use super::InternalError;
    use crate::intent_executor::strategy_executor::error::TransactionStrategyExecutionError;

    const TX_TOO_LARGE_SOLANA: &str = "base64 encoded too large";
    const TX_TOO_LARGE_MAGICBLOCK: &str =
        "base64 encoded solana_transaction::versioned::VersionedTransaction too large: 1684 bytes (max: encoded/raw 1644/1232)";

    fn make_send_transaction_error(message: &str) -> InternalError {
        let rpc_error = RpcClientError {
            request: Some(RpcRequest::SendTransaction),
            kind: Box::new(RpcClientErrorKind::RpcError(
                RpcError::RpcResponseError {
                    code: -32602,
                    message: message.to_string(),
                    data: RpcResponseErrorData::Empty,
                },
            )),
        };
        InternalError::MagicBlockRpcClientError(Box::new(
            MagicBlockRpcClientError::SendTransaction(Box::new(rpc_error)),
        ))
    }

    #[test]
    fn is_transaction_too_large_matches_solana_format() {
        let err = make_send_transaction_error(TX_TOO_LARGE_SOLANA);
        assert!(err.is_transaction_too_large());
    }

    #[test]
    fn is_transaction_too_large_matches_magicblock_format() {
        let err = make_send_transaction_error(TX_TOO_LARGE_MAGICBLOCK);
        assert!(err.is_transaction_too_large());
    }

    #[test]
    fn transaction_too_large_error_is_recoverable_by_two_stage() {
        let inner = make_send_transaction_error(TX_TOO_LARGE_MAGICBLOCK);
        let err =
            TransactionStrategyExecutionError::TransactionTooLargeError(inner);
        assert!(err.is_recoverable_by_two_stage());
    }

    #[test]
    fn transport_errors_are_transient() {
        let err = TransactionStrategyExecutionError::InternalError(
            make_send_transaction_error("connection reset"),
        );
        assert!(err.is_transient());

        let err = TransactionStrategyExecutionError::InternalError(
            InternalError::MagicBlockRpcClientError(Box::new(
                MagicBlockRpcClientError::SentTransactionError(
                    solana_transaction_error::TransactionError::BlockhashNotFound,
                    solana_signature::Signature::default(),
                ),
            )),
        );
        assert!(err.is_transient());
    }

    #[test]
    fn deterministic_errors_are_not_transient() {
        // Landed on-chain with an instruction error - deterministic
        let err = TransactionStrategyExecutionError::InternalError(
            InternalError::MagicBlockRpcClientError(Box::new(
                MagicBlockRpcClientError::SentTransactionError(
                    solana_transaction_error::TransactionError::InstructionError(
                        2,
                        solana_instruction::error::InstructionError::Custom(1),
                    ),
                    solana_signature::Signature::default(),
                ),
            )),
        );
        assert!(!err.is_transient());

        let err = TransactionStrategyExecutionError::InternalError(
            InternalError::SignerError(solana_signer::SignerError::Custom(
                "oops".to_string(),
            )),
        );
        assert!(!err.is_transient());

        assert!(!super::IntentExecutorError::EmptyIntentError.is_transient());
        assert!(!super::IntentExecutorError::FailedToFitError.is_transient());
    }

    #[test]
    fn builder_and_preparation_error_transience() {
        use crate::{
            tasks::{
                task_builder::TaskBuilderError,
                task_info_fetcher::TaskInfoFetcherError,
            },
            transaction_preparator::{
                delivery_preparator::{
                    DeliveryPreparatorError,
                    InternalError as DeliveryInternalError,
                },
                error::TransactionPreparatorError,
            },
        };

        fn transient_rpc_client_error() -> MagicBlockRpcClientError {
            MagicBlockRpcClientError::SendTransaction(Box::new(
                RpcClientError {
                    request: Some(RpcRequest::SendTransaction),
                    kind: Box::new(RpcClientErrorKind::Custom(
                        "io".to_string(),
                    )),
                },
            ))
        }
        fn transient_delivery_error() -> TransactionPreparatorError {
            TransactionPreparatorError::from(
                DeliveryPreparatorError::FailedToCreateALTError(
                    DeliveryInternalError::MagicBlockRpcClientError(Box::new(
                        transient_rpc_client_error(),
                    )),
                ),
            )
        }

        // RPC-side commit-id fetch failures are transient
        let err = super::IntentExecutorError::TaskBuilderError(
            TaskBuilderError::CommitTasksBuildError(
                TaskInfoFetcherError::MagicBlockRpcClientError(Box::new(
                    transient_rpc_client_error(),
                )),
            ),
        );
        assert!(err.is_transient());

        // Missing delegation metadata is deterministic
        let err = super::IntentExecutorError::TaskBuilderError(
            TaskBuilderError::MissingDelegationMetadata(
                solana_pubkey::Pubkey::new_unique(),
            ),
        );
        assert!(!err.is_transient());

        // Commit-stage delivery preparation RPC failures are transient
        let err = super::IntentExecutorError::FailedCommitPreparationError(
            transient_delivery_error(),
        );
        assert!(err.is_transient());

        // Oversized strategies are deterministic
        let err = super::IntentExecutorError::FailedCommitPreparationError(
            TransactionPreparatorError::FailedToFitError,
        );
        assert!(!err.is_transient());

        // Finalize preparation runs after a landed commit - always terminal
        let err = super::IntentExecutorError::FailedFinalizePreparationError(
            transient_delivery_error(),
        );
        assert!(!err.is_transient());
    }

    #[test]
    fn finalize_failure_after_landed_commit_is_not_transient() {
        let transient_err = || {
            TransactionStrategyExecutionError::InternalError(
                make_send_transaction_error("connection reset"),
            )
        };

        // Single-stage: nothing landed, safe to re-execute
        let err = super::IntentExecutorError::FailedToFinalizeError {
            err: transient_err(),
            commit_signature: None,
            finalize_signature: None,
        };
        assert!(err.is_transient());

        // Two-stage: commit landed, re-execution would commit again
        let err = super::IntentExecutorError::FailedToFinalizeError {
            err: transient_err(),
            commit_signature: Some(solana_signature::Signature::default()),
            finalize_signature: None,
        };
        assert!(!err.is_transient());
    }

    #[test]
    fn unrelated_internal_errors_do_not_trigger_single_stage_split() {
        let err = TransactionStrategyExecutionError::InternalError(
            InternalError::MagicBlockRpcClientError(Box::new(
                MagicBlockRpcClientError::LookupTableDeserialize(
                    solana_instruction::error::InstructionError::GenericError,
                ),
            )),
        );
        assert!(!err.is_recoverable_by_two_stage());
    }
}
