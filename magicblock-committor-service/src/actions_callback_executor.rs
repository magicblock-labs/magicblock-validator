pub use magicblock_core::traits::{ActionError, ActionResult};

use crate::intent_executor::error::{
    IntentExecutorError, TransactionStrategyExecutionError,
};

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
