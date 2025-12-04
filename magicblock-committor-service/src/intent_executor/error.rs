use log::error;
use magicblock_metrics::metrics;
use magicblock_rpc_client::{
    utils::TransactionErrorMapper, MagicBlockRpcClientError,
};
use solana_sdk::{
    instruction::InstructionError,
    signature::{Signature, SignerError},
    transaction::TransactionError,
};

use crate::{
    tasks::{
        task_builder::TaskBuilderError, task_strategist::TaskStrategistError,
        BaseTask, TaskType,
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
}

#[derive(thiserror::Error, Debug)]
pub enum IntentExecutorError {
    #[error("EmptyIntentError")]
    EmptyIntentError,
    #[error("Failed to fit in single TX")]
    FailedToFitError,
    #[error("SignerError: {0}")]
    SignerError(#[from] SignerError),
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
    #[error("User supplied actions are ill-formed: {0}. {:?}", .1)]
    ActionsError(#[source] TransactionError, Option<Signature>),
    #[error("Invalid undelegation: {0}. {:?}", .1)]
    UndelegationError(#[source] TransactionError, Option<Signature>),
    #[error("Accounts committed with an invalid Commit id: {0}. {:?}", .1)]
    CommitIDError(#[source] TransactionError, Option<Signature>),
    #[error("Max instruction trace length exceeded: {0}. {:?}", .1)]
    CpiLimitError(#[source] TransactionError, Option<Signature>),
    #[error("InternalError: {0}")]
    InternalError(#[from] InternalError),
}

impl From<MagicBlockRpcClientError> for TransactionStrategyExecutionError {
    fn from(value: MagicBlockRpcClientError) -> Self {
        Self::InternalError(InternalError::from(value))
    }
}

impl TransactionStrategyExecutionError {
    pub fn is_cpi_limit_error(&self) -> bool {
        matches!(self, Self::CpiLimitError(_, _))
    }

    pub fn signature(&self) -> Option<Signature> {
        match self {
            Self::InternalError(err) => err.signature(),
            Self::CommitIDError(_, signature)
            | Self::ActionsError(_, signature)
            | Self::UndelegationError(_, signature)
            | Self::CpiLimitError(_, signature) => *signature,
        }
    }

    /// Convert [`TransactionError`] into known errors that can be handled
    /// Otherwise return original [`TransactionError`]
    /// [`TransactionStrategyExecutionError`]
    pub fn try_from_transaction_error(
        err: TransactionError,
        signature: Option<Signature>,
        tasks: &[Box<dyn BaseTask>],
    ) -> Result<Self, TransactionError> {
        // There's always 2 budget instructions in front
        const OFFSET: u8 = 2;
        const NONCE_OUT_OF_ORDER: u32 =
            dlp::error::DlpError::NonceOutOfOrder as u32;

        match err {
            // Some tx may use too much CPIs and we can handle it in certain cases
            transaction_err @ TransactionError::InstructionError(
                _,
                InstructionError::MaxInstructionTraceLengthExceeded,
            ) => Ok(TransactionStrategyExecutionError::CpiLimitError(
                transaction_err,
                signature,
            )),
            // Map per-task InstructionError into CommitID / Actions / Undelegation errors when possible
            TransactionError::InstructionError(index, instruction_err) => {
                let tx_err_helper = |instruction_err| -> TransactionError {
                    TransactionError::InstructionError(index, instruction_err)
                };
                let Some(action_index) = index.checked_sub(OFFSET) else {
                    return Err(tx_err_helper(instruction_err));
                };

                let Some(task_type) = tasks
                    .get(action_index as usize)
                    .map(|task| task.task_type())
                else {
                    return Err(tx_err_helper(instruction_err));
                };

                match (task_type, instruction_err) {
                    (
                        TaskType::Commit,
                        InstructionError::Custom(NONCE_OUT_OF_ORDER),
                    ) => Ok(TransactionStrategyExecutionError::CommitIDError(
                        tx_err_helper(InstructionError::Custom(
                            NONCE_OUT_OF_ORDER,
                        )),
                        signature,
                    )),
                    (TaskType::Action, instruction_err) => {
                        Ok(TransactionStrategyExecutionError::ActionsError(
                            tx_err_helper(instruction_err),
                            signature,
                        ))
                    }
                    (TaskType::Undelegate, instruction_err) => Ok(
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
                error!(
                    "Message execution failed and we can not handle it: {}",
                    err
                );
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
            Self::CommitIDError(_, _) => "commit_nonce_failed",
            Self::UndelegationError(_, _) => "undelegation_failed",
            _ => "failed",
        }
    }
}

pub(crate) struct IntentTransactionErrorMapper<'a> {
    pub tasks: &'a [Box<dyn BaseTask>],
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

pub type IntentExecutorResult<T, E = IntentExecutorError> = Result<T, E>;
