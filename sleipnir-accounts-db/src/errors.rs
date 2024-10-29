use thiserror::Error;

#[derive(Error, Debug, PartialEq, Eq)]
pub enum MatchAccountOwnerError {
    #[error("The account owner does not match with the provided list")]
    NoMatch,
    #[error("Unable to load the account")]
    UnableToLoad,
}

pub type AccountsDbResult<T> = std::result::Result<T, AccountsDbError>;

#[derive(Error, Debug)]
pub enum AccountsDbError {
    #[error("fs extra error: {0}")]
    FsExtraError(#[from] fs_extra::error::Error),
    #[error("io error: {0}")]
    IOError(#[from] std::io::Error),
}
