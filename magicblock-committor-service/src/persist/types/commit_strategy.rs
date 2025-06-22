#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CommitStrategy {
    /// The commit strategy is not known yet
    Undetermined,
    /// Args without the use of a lookup table
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
            Undetermined => "Undetermined",
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

impl From<&str> for CommitStrategy {
    fn from(value: &str) -> Self {
        match value {
            "Args" => Self::Args,
            "ArgsWithLookupTable" => Self::ArgsWithLookupTable,
            "FromBuffer" => Self::FromBuffer,
            "FromBufferWithLookupTable" => Self::FromBufferWithLookupTable,
            _ => Self::Undetermined,
        }
    }
}
