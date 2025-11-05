use crate::persist::error::CommitPersistError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum CommitStrategy {
    /// Args without the use of a lookup table
    #[default]
    StateArgs,
    /// Args with the use of a lookup table
    StateArgsWithLookupTable,
    /// Buffer and chunks which has the most overhead
    StateBuffer,
    /// Buffer and chunks with the use of a lookup table
    StateBufferWithLookupTable,
}

impl CommitStrategy {
    pub fn args(use_lookup: bool) -> Self {
        if use_lookup {
            Self::StateArgsWithLookupTable
        } else {
            Self::StateArgs
        }
    }

    pub fn as_str(&self) -> &str {
        use CommitStrategy::*;
        match self {
            StateArgs => "StateArgs",
            StateArgsWithLookupTable => "StateArgsWithLookupTable",
            StateBuffer => "StateBuffer",
            StateBufferWithLookupTable => "StateBufferWithLookupTable",
        }
    }

    pub fn uses_lookup(&self) -> bool {
        matches!(
            self,
            CommitStrategy::StateArgsWithLookupTable
                | CommitStrategy::StateBufferWithLookupTable
        )
    }
}

impl TryFrom<&str> for CommitStrategy {
    type Error = CommitPersistError;
    fn try_from(value: &str) -> Result<Self, CommitPersistError> {
        match value {
            "Args" | "StateArgs" => Ok(Self::StateArgs),
            "ArgsWithLookupTable" | "StateArgsWithLookupTable" => {
                Ok(Self::StateArgsWithLookupTable)
            }
            "FromBuffer" | "StateBuffer" => Ok(Self::StateBuffer),
            "FromBufferWithLookupTable" | "StateBufferWithLookupTable" => {
                Ok(Self::StateBufferWithLookupTable)
            }
            _ => Err(CommitPersistError::InvalidCommitStrategy(
                value.to_string(),
            )),
        }
    }
}
