use thiserror::Error;

pub type AccountsResult<T> = std::result::Result<T, AccountsError>;

#[derive(Error, Debug)]
pub enum AccountsError {
    #[error("TranswiseError")]
    TranswiseError(#[from] conjunto_transwise::errors::TranswiseError),

    #[error("MutatorError")]
    MutatorError(#[from] sleipnir_mutator::errors::MutatorError),

    #[error("UrlParseError")]
    UrlParseError(#[from] url::ParseError),

    #[error("SanitizeError")]
    SanitizeError(#[from] solana_sdk::sanitize::SanitizeError),

    #[error("TransactionError")]
    TransactionError(#[from] solana_sdk::transaction::TransactionError),

    #[error("InvalidRpcUrl '{0}'")]
    InvalidRpcUrl(String),

    #[error("FailedToUpdateUrlScheme")]
    FailedToUpdateUrlScheme,

    #[error("FailedToUpdateUrlPort")]
    FailedToUpdateUrlPort,

    #[error("FailedToGetLatestBlockhash '{0}'")]
    FailedToGetLatestBlockhash(String),

    #[error("FailedToSendTransaction '{0}'")]
    FailedToSendTransaction(String),

    #[error("FailedToConfirmTransaction '{0}'")]
    FailedToConfirmTransaction(String),

    #[error("FailedToFetchAccount '{0}'")]
    FailedToFetchAccount(String),

    #[error("FailedToResolveAccount '{0}'")]
    FailedToResolveAccount(String),
}
