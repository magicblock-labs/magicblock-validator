use engine::EngineError;
use solana_pubkey::Pubkey;
use thiserror::Error;

pub type ClonerResult<T> = std::result::Result<T, ClonerError>;

#[derive(Debug, Error)]
pub enum ClonerError {
    #[error(transparent)]
    BincodeError(#[from] bincode::Error),
    #[error(transparent)]
    TryFromIntError(#[from] std::num::TryFromIntError),
    #[error(transparent)]
    SignerError(#[from] solana_signer::SignerError),
    #[error(transparent)]
    TransactionError(#[from] solana_transaction_error::TransactionError),
    #[error(transparent)]
    RemoteAccountProviderError(
        #[from] crate::remote_account_provider::RemoteAccountProviderError,
    ),
    #[error("CommittorServiceError {0}")]
    CommittorServiceError(String),

    #[error("engine error: {0}")]
    Engine(#[from] EngineError),

    #[error(
        "Clone transaction for account {pubkey} is too large: {size} bytes (max {max_size} bytes)"
    )]
    CloneTransactionTooLarge {
        pubkey: Pubkey,
        size: usize,
        max_size: usize,
    },

    #[error("Failed to clone regular account {0} : {1:?}")]
    FailedToCloneRegularAccount(Pubkey, Box<ClonerError>),

    #[error("Failed to create clone program transaction {0} : {1:?}")]
    FailedToCreateCloneProgramTransaction(Pubkey, Box<ClonerError>),

    #[error("Failed to clone program {0} : {1:?}")]
    FailedToCloneProgram(Pubkey, Box<ClonerError>),

    #[error(
        "Failed to clone and schedule undelegation for account {0} : {1:?}"
    )]
    FailedToCloneAndScheduleUndelegation(Pubkey, Box<ClonerError>),

    #[error("Failed to evict account {0} : {1:?}")]
    FailedToEvictAccount(Pubkey, Box<ClonerError>),

    #[error("Failed to schedule undelegation {0} : {1:?}")]
    FailedToScheduleUndelegation(Pubkey, Box<ClonerError>),
}

impl ClonerError {
    /// Rescue requires confirmed execution failure or rejection before submission.
    /// Infrastructure and exceptional completion-task errors do not prove rollback.
    pub(crate) fn allows_rescue(&self) -> bool {
        match self {
            Self::FailedToCloneRegularAccount(_, error) => {
                error.allows_rescue()
            }
            Self::Engine(
                EngineError::TransactionExecution(_)
                | EngineError::TransactionCompile(_)
                | EngineError::Sanitization(_)
                | EngineError::Signature(_)
                | EngineError::SignatureVerification
                | EngineError::Serde(_),
            ) => true,
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use solana_transaction_error::TransactionError;

    use super::*;

    /// Proves only typed rejection/execution errors allow rescue, including
    /// through the regular-account wrapper; error text cannot authorize it.
    #[test]
    fn rescue_eligibility_requires_definitive_failure() {
        for error in [
            EngineError::Sanitization(
                keeper::TransactionView::try_new_sanitized(
                    Vec::new().into(),
                    true,
                )
                .unwrap_err(),
            ),
            EngineError::Serde(wincode::WriteError::Custom("rejected").into()),
            EngineError::TransactionExecution(
                TransactionError::AccountNotFound,
            ),
            EngineError::TransactionCompile(
                solana_message::CompileError::AccountIndexOverflow,
            ),
            EngineError::Signature(
                solana_signer::SignerError::NotEnoughSigners,
            ),
            EngineError::SignatureVerification,
        ] {
            let error = ClonerError::Engine(error);
            assert!(error.allows_rescue());
            assert!(
                ClonerError::FailedToCloneRegularAccount(
                    Pubkey::new_unique(),
                    Box::new(error),
                )
                .allows_rescue()
            );
        }
        for error in [
            EngineError::ShuttingDown,
            EngineError::ServiceUnavailable(
                nucleus::shutdown::Service::Sequencer,
            ),
            EngineError::Internal("transaction execution failed".into()),
        ] {
            let error = ClonerError::Engine(error);
            assert!(!error.allows_rescue());
            assert!(
                !ClonerError::FailedToCloneRegularAccount(
                    Pubkey::new_unique(),
                    Box::new(error),
                )
                .allows_rescue()
            );
        }
        assert!(
            !ClonerError::TransactionError(TransactionError::AccountNotFound)
                .allows_rescue()
        );
    }

    /// Proves an exceptional completion-task cancellation is not rollback evidence.
    #[tokio::test]
    async fn rescue_eligibility_rejects_task_failure() {
        let task = tokio::spawn(std::future::pending::<()>());
        task.abort();
        let error = ClonerError::FailedToCloneRegularAccount(
            Pubkey::new_unique(),
            Box::new(ClonerError::Engine(EngineError::Task(
                task.await.unwrap_err(),
            ))),
        );
        assert!(!error.allows_rescue());
    }
}
