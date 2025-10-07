use log::error;
use magicblock_rpc_client::MagicBlockRpcClientError;
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
    MagicBlockRpcClientError(#[from] MagicBlockRpcClientError),
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
    #[error("User supplied action are ill-formed!")]
    ActionsError,
    #[error("Accounts committed with an invalid Commit id")]
    CommitIDError,
    #[error("Max instruction trace length exceeded")]
    CpiLimitError,
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
        err: InternalError,
        signature: Option<Signature>,
    },
    #[error("FailedToFinalizeError: {err}")]
    FailedToFinalizeError {
        #[source]
        err: InternalError,
        commit_signature: Option<Signature>,
        finalize_signature: Option<Signature>,
    },
    #[error("FailedCommitPreparationError: {0}")]
    FailedCommitPreparationError(#[source] TransactionPreparatorError),
    #[error("FailedFinalizePreparationError: {0}")]
    FailedFinalizePreparationError(#[source] TransactionPreparatorError),
}

impl IntentExecutorError {
    pub fn is_cpi_limit_error(&self) -> bool {
        matches!(self, IntentExecutorError::CpiLimitError)
    }

    pub fn from_strategy_execution_error<F>(
        error: TransactionStrategyExecutionError,
        converter: F,
    ) -> IntentExecutorError
    where
        F: FnOnce(InternalError) -> IntentExecutorError,
    {
        match error {
            TransactionStrategyExecutionError::ActionsError => {
                IntentExecutorError::ActionsError
            }
            TransactionStrategyExecutionError::CpiLimitError => {
                IntentExecutorError::CpiLimitError
            }
            TransactionStrategyExecutionError::CommitIDError => {
                IntentExecutorError::CommitIDError
            }
            TransactionStrategyExecutionError::InternalError(err) => {
                converter(err)
            }
        }
    }
}

/// Those are the errors that may occur during Commit/Finalize stages on Base layer
#[derive(thiserror::Error, Debug)]
pub enum TransactionStrategyExecutionError {
    #[error("User supplied action are ill-formed!")]
    ActionsError,
    #[error("Accounts committed with an invalid Commit id")]
    CommitIDError,
    #[error("Max instruction trace length exceeded")]
    CpiLimitError,
    #[error("InternalError: {0}")]
    InternalError(#[from] InternalError),
}

impl TransactionStrategyExecutionError {
    /// Convert [`TransactionError`] into known errors that can be handled
    /// [`TransactionStrategyExecutionError`]
    pub fn from_transaction_error(
        err: TransactionError,
        tasks: &[Box<dyn BaseTask>],
        map: impl FnOnce(TransactionError) -> MagicBlockRpcClientError,
    ) -> Self {
        // There's always 2 budget instructions in front
        const OFFSET: u8 = 2;
        const OUTDATED_SLOT: u32 = dlp::error::DlpError::OutdatedSlot as u32;

        match err {
            // Filter CommitIdError by custom error code
            TransactionError::InstructionError(
                _,
                InstructionError::Custom(OUTDATED_SLOT),
            ) => TransactionStrategyExecutionError::CommitIDError,
            // Some tx may use too much CPIs and we can handle it in certain cases
            TransactionError::InstructionError(
                _,
                InstructionError::MaxInstructionTraceLengthExceeded,
            ) => TransactionStrategyExecutionError::CpiLimitError,
            // Filter ActionError, we can attempt recovery by stripping away actions
            TransactionError::InstructionError(index, ix_err) => {
                let transaction_error =
                    TransactionError::InstructionError(index, ix_err);
                let internal_err =
                    TransactionStrategyExecutionError::InternalError(
                        InternalError::MagicBlockRpcClientError(map(
                            transaction_error,
                        )),
                    );

                let Some(action_index) = index.checked_sub(OFFSET) else {
                    return internal_err;
                };

                // If index exists and corresponds to Action - return ActionsError
                if let Some(TaskType::Action) = tasks
                    .get(action_index as usize)
                    .map(|task| task.task_type())
                {
                    TransactionStrategyExecutionError::ActionsError
                } else {
                    internal_err
                }
            }
            // This means transaction failed to other reasons that we don't handle - propagate
            err => {
                error!(
                    "Message execution failed and we can not handle it: {}",
                    err
                );
                TransactionStrategyExecutionError::InternalError(
                    InternalError::MagicBlockRpcClientError(map(err)),
                )
            }
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
