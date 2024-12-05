use std::collections::HashSet;

use magicblock_account_cloner::{
    AccountClonerError, AccountClonerUnclonableReason,
};
use solana_sdk::pubkey::Pubkey;
use thiserror::Error;

pub type AccountsResult<T> = std::result::Result<T, AccountsError>;

#[derive(Error, Debug)]
pub enum AccountsError {
    #[error("TranswiseError")]
    TranswiseError(#[from] Box<conjunto_transwise::errors::TranswiseError>),

    #[error("UrlParseError")]
    UrlParseError(#[from] Box<url::ParseError>),

    #[error("TransactionError")]
    TransactionError(#[from] Box<solana_sdk::transaction::TransactionError>),

    #[error("AccountClonerError")]
    AccountClonerError(#[from] AccountClonerError),

    #[error("UnclonableAccountUsedAsWritableInEphemeral '{0}' ('{1:?}')")]
    UnclonableAccountUsedAsWritableInEphemeral(
        Pubkey,
        AccountClonerUnclonableReason,
    ),

    #[error("InvalidRpcUrl '{0}'")]
    InvalidRpcUrl(String),

    #[error("FailedToUpdateUrlScheme")]
    FailedToUpdateUrlScheme,

    #[error("FailedToUpdateUrlPort")]
    FailedToUpdateUrlPort,

    #[error("FailedToGetLatestBlockhash '{0}'")]
    FailedToGetLatestBlockhash(String),

    #[error("FailedToGetReimbursementAddress '{0}'")]
    FailedToGetReimbursementAddress(String),

    #[error("FailedToSendCommitTransaction '{0}'")]
    FailedToSendCommitTransaction(String, HashSet<Pubkey>, HashSet<Pubkey>),

    #[error("Too many committees: {0}")]
    TooManyCommittees(usize),
}
