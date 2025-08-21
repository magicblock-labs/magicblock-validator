use crate::persist::error::CommitPersistError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum CommitStrategy {
    /// Args without the use of a lookup table
    #[default]
    Args,
    /// Args with the use of a lookup table
    ArgsWithLookupTable,
    /// Buffer and chunks which has the most overhead
    FromBuffer,
    /// Buffer and chunks with the use of a lookup table
    FromBufferWithLookupTable,
}

impl CommitStrategy {
    pub fn args(use_lookup: bool) -> Self {
        if use_lookup {
            Self::ArgsWithLookupTable
        } else {
            Self::Args
        }
    }

    pub fn as_str(&self) -> &str {
        use CommitStrategy::*;
        match self {
            Args => "Args",
            ArgsWithLookupTable => "ArgsWithLookupTable",
            FromBuffer => "FromBuffer",
            FromBufferWithLookupTable => "FromBufferWithLookupTable",
        }
    }

    pub fn uses_lookup(&self) -> bool {
        matches!(
            self,
            CommitStrategy::ArgsWithLookupTable
                | CommitStrategy::FromBufferWithLookupTable
        )
    }
}

impl TryFrom<&str> for CommitStrategy {
    type Error = CommitPersistError;
    fn try_from(value: &str) -> Result<Self, CommitPersistError> {
        match value {
            "Args" => Ok(Self::Args),
            "ArgsWithLookupTable" => Ok(Self::ArgsWithLookupTable),
            "FromBuffer" => Ok(Self::FromBuffer),
            "FromBufferWithLookupTable" => Ok(Self::FromBufferWithLookupTable),
            _ => Err(CommitPersistError::InvalidCommitStrategy(
                value.to_string(),
            )),
        }
    }
}
