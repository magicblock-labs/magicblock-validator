use magicblock_core::traits::ActionError;
use magicblock_metrics::metrics;
use magicblock_rpc_client::{
    utils::TransactionErrorMapper, MagicBlockRpcClientError,
};
use solana_instruction::error::InstructionError;
use solana_rpc_client_api::{
    client_error::{Error as RpcClientError, ErrorKind as RpcClientErrorKind},
    request::{RpcError, RpcRequest},
};
use solana_signature::Signature;
use solana_signer::SignerError;
use solana_transaction_error::TransactionError;
use tracing::error;

use crate::{
    outbox_client::InternalOutboxClientError,
    tasks::{
        task_builder::TaskBuilderError, task_strategist::TaskStrategistError,
        BaseTaskImpl,
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
    // TODO(edwin): remove once proper retries introduced
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

    pub fn signatures(&self) -> Option<(Signature, Option<Signature>)> {
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
            | IntentExecutorError::SignerError(_) => None,
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

/// Those are the errors that may occur during Commit/Finalize stages on Base layer
#[derive(thiserror::Error, Debug)]
pub enum TransactionStrategyExecutionError {
    #[error("User supplied actions are ill-formed: {0}. {1:?}")]
    ActionsError(#[source] TransactionError, Option<Signature>),
    #[error("Invalid undelegation: {0}. {1:?}")]
    UndelegationError(#[source] TransactionError, Option<Signature>),
    #[error("Accounts committed with an invalid Commit id: {0}. {1:?}")]
    CommitIDError(#[source] TransactionError, Option<Signature>),
    #[error("Max instruction trace length exceeded: {0}. {1:?}")]
    CpiLimitError(#[source] TransactionError, Option<Signature>),
    #[error("Loaded accounts data size exceeded: {0}. {1:?}")]
    LoadedAccountsDataSizeExceeded(
        #[source] TransactionError,
        Option<Signature>,
    ),
    #[error("Unfinalized account error: {0}, {1:?}")]
    UnfinalizedAccountError(#[source] TransactionError, Option<Signature>),
    #[error("Transaction too large to send over the wire: {0}")]
    TransactionTooLargeError(#[source] InternalError),
    #[error("InternalError: {0}")]
    InternalError(#[from] InternalError),
}

impl From<MagicBlockRpcClientError> for TransactionStrategyExecutionError {
    fn from(value: MagicBlockRpcClientError) -> Self {
        Self::InternalError(InternalError::from(value))
    }
}

impl TransactionStrategyExecutionError {
    /// Number of compute budget instructions prepended to every transaction.
    /// Used to map instruction indices back to task indices.
    const TASK_OFFSET: u8 = 2;

    pub fn is_cpi_limit_error(&self) -> bool {
        matches!(
            self,
            Self::CpiLimitError(_, _)
                | Self::LoadedAccountsDataSizeExceeded(_, _)
        )
    }

    pub fn is_recoverable_by_two_stage(&self) -> bool {
        self.is_cpi_limit_error()
            || matches!(self, Self::TransactionTooLargeError(_))
    }

    pub fn task_index(&self) -> Option<u8> {
        match self {
            Self::CommitIDError(
                TransactionError::InstructionError(index, _),
                _,
            )
            | Self::ActionsError(
                TransactionError::InstructionError(index, _),
                _,
            )
            | Self::UndelegationError(
                TransactionError::InstructionError(index, _),
                _,
            )
            | Self::UnfinalizedAccountError(
                TransactionError::InstructionError(index, _),
                _,
            )
            | Self::CpiLimitError(
                TransactionError::InstructionError(index, _),
                _,
            ) => index.checked_sub(Self::TASK_OFFSET),
            _ => None,
        }
    }

    pub fn signature(&self) -> Option<Signature> {
        match self {
            Self::InternalError(err) => err.signature(),
            Self::TransactionTooLargeError(err) => err.signature(),
            Self::CommitIDError(_, signature)
            | Self::ActionsError(_, signature)
            | Self::UndelegationError(_, signature)
            | Self::UnfinalizedAccountError(_, signature)
            | Self::CpiLimitError(_, signature)
            | Self::LoadedAccountsDataSizeExceeded(_, signature) => *signature,
        }
    }

    /// Convert [`TransactionError`] into known errors that can be handled
    /// Otherwise return original [`TransactionError`]
    /// [`TransactionStrategyExecutionError`]
    pub fn try_from_transaction_error(
        err: TransactionError,
        signature: Option<Signature>,
        tasks: &[BaseTaskImpl],
    ) -> Result<Self, TransactionError> {
        // Commit Nonce order error
        const NONCE_OUT_OF_ORDER: u32 =
            dlp_api::error::DlpError::NonceOutOfOrder as u32;
        // Errors when commit state already exists
        const COMMIT_STATE_INVALID_ACCOUNT_OWNER: u32 =
            dlp_api::error::DlpError::CommitStateInvalidAccountOwner as u32;
        const COMMIT_STATE_ALREADY_INITIALIZED: u32 =
            dlp_api::error::DlpError::CommitStateAlreadyInitialized as u32;
        const COMMIT_RECORD_INVALID_ACCOUNT_OWNER: u32 =
            dlp_api::error::DlpError::CommitRecordInvalidAccountOwner as u32;
        const COMMIT_RECORD_ALREADY_INITIALIZED: u32 =
            dlp_api::error::DlpError::CommitRecordAlreadyInitialized as u32;

        match err {
            // Some tx may use too much CPIs and we can handle it in certain cases
            transaction_err @ TransactionError::InstructionError(
                _,
                InstructionError::MaxInstructionTraceLengthExceeded,
            ) => Ok(TransactionStrategyExecutionError::CpiLimitError(
                transaction_err,
                signature,
            )),
            err @ TransactionError::MaxLoadedAccountsDataSizeExceeded => {
                Ok(TransactionStrategyExecutionError::LoadedAccountsDataSizeExceeded(
                    err,
                    signature,
                ))
            }
            // Map per-task InstructionError into CommitID / Actions / Undelegation errors when possible
            TransactionError::InstructionError(index, instruction_err) => {
                let tx_err_helper = |instruction_err| -> TransactionError {
                    TransactionError::InstructionError(index, instruction_err)
                };
                let Some(action_index) = index.checked_sub(Self::TASK_OFFSET)
                else {
                    return Err(tx_err_helper(instruction_err));
                };

                let Some(task) = tasks.get(action_index as usize) else {
                    return Err(tx_err_helper(instruction_err));
                };

                match (task, instruction_err) {
                    (
                        BaseTaskImpl::Commit(_)
                        | BaseTaskImpl::CommitFinalize(_),
                        instruction_err,
                    ) => match instruction_err {
                        InstructionError::Custom(NONCE_OUT_OF_ORDER) => Ok(
                            TransactionStrategyExecutionError::CommitIDError(
                                tx_err_helper(InstructionError::Custom(
                                    NONCE_OUT_OF_ORDER,
                                )),
                                signature,
                            ),
                        ),
                        instruction_err @ (InstructionError::Custom(
                            COMMIT_STATE_INVALID_ACCOUNT_OWNER,
                        )
                        | InstructionError::Custom(
                            COMMIT_STATE_ALREADY_INITIALIZED,
                        )
                        | InstructionError::Custom(
                            COMMIT_RECORD_INVALID_ACCOUNT_OWNER,
                        )
                        | InstructionError::Custom(
                            COMMIT_RECORD_ALREADY_INITIALIZED,
                        )) => {
                            Ok(TransactionStrategyExecutionError::UnfinalizedAccountError(
                                tx_err_helper(instruction_err),
                                signature
                            ))
                        }
                        err => Err(tx_err_helper(err)),
                    },
                    (BaseTaskImpl::BaseAction(_), instruction_err) => {
                        Ok(TransactionStrategyExecutionError::ActionsError(
                            tx_err_helper(instruction_err),
                            signature,
                        ))
                    }
                    (BaseTaskImpl::Undelegate(_), instruction_err) => Ok(
                        TransactionStrategyExecutionError::UndelegationError(
                            tx_err_helper(instruction_err),
                            signature,
                        ),
                    ),
                    (_, instruction_err) => Err(tx_err_helper(instruction_err)),
                }
            }
            // This means transaction failed to other reasons that we don't handle - propagate
            err => {
                error!(error = ?err, "Message execution failed");
                Err(err)
            }
        }
    }
}

impl metrics::LabelValue for TransactionStrategyExecutionError {
    fn value(&self) -> &str {
        match self {
            Self::ActionsError(_, _) => "actions_failed",
            Self::CpiLimitError(_, _) => "cpi_limit_failed",
            Self::LoadedAccountsDataSizeExceeded(_, _) => {
                "loaded_accounts_data_limit_exceeded"
            }
            Self::CommitIDError(_, _) => "commit_nonce_failed",
            Self::UndelegationError(_, _) => "undelegation_failed",
            Self::UnfinalizedAccountError(_, _) => "unfinalized_account_failed",
            Self::TransactionTooLargeError(_) => "transaction_too_large",
            _ => "failed",
        }
    }
}

pub(crate) struct IntentTransactionErrorMapper<'a> {
    pub tasks: &'a [BaseTaskImpl],
}
impl TransactionErrorMapper for IntentTransactionErrorMapper<'_> {
    type ExecutionError = TransactionStrategyExecutionError;
    fn try_map(
        &self,
        error: TransactionError,
        signature: Option<Signature>,
    ) -> Result<Self::ExecutionError, TransactionError> {
        TransactionStrategyExecutionError::try_from_transaction_error(
            error, signature, self.tasks,
        )
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

impl From<&TransactionStrategyExecutionError> for ActionError {
    fn from(value: &TransactionStrategyExecutionError) -> Self {
        if let TransactionStrategyExecutionError::ActionsError(err, signature) =
            value
        {
            Self::ActionsError(err.clone(), *signature)
        } else {
            Self::IntentFailedError(value.to_string())
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

    use super::{InternalError, TransactionStrategyExecutionError};

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
