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

    /// Args without the use of a lookup table
    DiffArgs,
    /// Args with the use of a lookup table
    DiffArgsWithLookupTable,
    /// Buffer and chunks which has the most overhead
    DiffBuffer,
    /// Buffer and chunks with the use of a lookup table
    DiffBufferWithLookupTable,
}

impl CommitStrategy {
    pub fn as_str(&self) -> &str {
        use CommitStrategy::*;
        match self {
            StateArgs => "StateArgs",
            StateArgsWithLookupTable => "StateArgsWithLookupTable",
            StateBuffer => "StateBuffer",
            StateBufferWithLookupTable => "StateBufferWithLookupTable",
            DiffArgs => "DiffArgs",
            DiffArgsWithLookupTable => "DiffArgsWithLookupTable",
            DiffBuffer => "DiffBuffer",
            DiffBufferWithLookupTable => "DiffBufferWithLookupTable",
        }
    }

    pub fn uses_lookup(&self) -> bool {
        matches!(
            self,
            CommitStrategy::StateArgsWithLookupTable
                | CommitStrategy::StateBufferWithLookupTable
                | CommitStrategy::DiffArgsWithLookupTable
                | CommitStrategy::DiffBufferWithLookupTable
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
            "DiffArgs" => Ok(Self::DiffArgs),
            "DiffArgsWithLookupTable" => Ok(Self::DiffArgsWithLookupTable),
            "DiffBuffer" => Ok(Self::DiffBuffer),
            "DiffBufferWithLookupTable" => Ok(Self::DiffBufferWithLookupTable),
            _ => Err(CommitPersistError::InvalidCommitStrategy(
                value.to_string(),
            )),
        }
    }
}
