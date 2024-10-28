use thiserror::Error;

#[derive(Error, Debug, PartialEq, Eq)]
pub enum MatchAccountOwnerError {
    #[error("The account owner does not match with the provided list")]
    NoMatch,
    #[error("Unable to load the account")]
    UnableToLoad,
}

#[derive(Error, Debug)]
pub enum AccountsDbError {
    #[error("fs extra error: {0}")]
    FsExtraError(#[from] fs_extra::error::Error),
}
