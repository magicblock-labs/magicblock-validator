use sleipnir_account_cloner::AccountClonerError;
use solana_sdk::pubkey::Pubkey;
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

    #[error("AccountClonerError")]
    AccountClonerError(#[from] AccountClonerError),

    #[error("UnclonableAccountUsedAsWritableInEphemeral '{0}'")]
    UnclonableAccountUsedAsWritableInEphemeral(Pubkey),

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

    #[error("Too many committees: {0}")]
    TooManyCommittees(usize),
}
