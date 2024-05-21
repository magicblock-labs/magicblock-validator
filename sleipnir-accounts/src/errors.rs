use thiserror::Error;

pub type AccountsResult<T> = std::result::Result<T, AccountsError>;

#[derive(Error, Debug)]
pub enum AccountsError {
    #[error("TranswiseError")]
    TranswiseError(#[from] conjunto_transwise::errors::TranswiseError),
    #[error("MutatorError")]
    MutatorError(#[from] sleipnir_mutator::errors::MutatorError),
}
