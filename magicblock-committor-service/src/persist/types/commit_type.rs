use crate::persist::error::CommitPersistError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommitType {
    EmptyAccount,
    DataAccount,
}

impl TryFrom<&str> for CommitType {
    type Error = CommitPersistError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "EmptyAccount" => Ok(CommitType::EmptyAccount),
            "DataAccount" => Ok(CommitType::DataAccount),
            _ => Err(CommitPersistError::InvalidCommitType(value.to_string())),
        }
    }
}

impl CommitType {
    pub fn as_str(&self) -> &str {
        match self {
            CommitType::EmptyAccount => "EmptyAccount",
            CommitType::DataAccount => "DataAccount",
        }
    }
}
