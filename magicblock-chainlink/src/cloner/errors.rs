use thiserror::Error;

pub type ClonerResult<T> = std::result::Result<T, ClonerError>;

#[derive(Debug, Error)]
pub enum ClonerError {
    #[error(transparent)]
    BincodeError(#[from] bincode::Error),
    #[error(transparent)]
    TryFromIntError(#[from] std::num::TryFromIntError),
    #[error(transparent)]
    TransactionError(#[from] solana_transaction_error::TransactionError),
}
